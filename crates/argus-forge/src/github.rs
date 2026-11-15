//! GitHub (and GitHub Enterprise) via the REST API.

use std::process::Command;
use std::sync::OnceLock;

use argus_core::forge::{
    Checks, Forge, ForgeError, ParentInfo, Protocol, PullRequest, PullState, RemoteRepo, RepoInfo,
    Review, Whereabouts,
};
use argus_core::identity::RepoId;
use serde::Deserialize;

pub struct Response {
    pub status: u16,
    pub body: String,
    /// Raw `Link` header, for pagination.
    pub link: Option<String>,
}

/// HTTP seam so the client can be tested without the network.
pub trait Http: Send + Sync {
    fn get(&self, url: &str, token: Option<&str>) -> Result<Response, String>;
    /// POST a JSON body (the GraphQL API).
    fn post(&self, _url: &str, _token: Option<&str>, _body: &str) -> Result<Response, String> {
        Err("POST isn't supported here".into())
    }
}

struct Ureq(ureq::Agent);

impl Http for Ureq {
    fn get(&self, url: &str, token: Option<&str>) -> Result<Response, String> {
        let mut req = self
            .0
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", concat!("argus/", env!("CARGO_PKG_VERSION")));
        if let Some(t) = token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.call().map_err(|e| e.to_string())?;
        let link = resp
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(Response { status, body, link })
    }

    fn post(&self, url: &str, token: Option<&str>, body: &str) -> Result<Response, String> {
        let mut req = self
            .0
            .post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", concat!("argus/", env!("CARGO_PKG_VERSION")));
        if let Some(t) = token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.send(body).map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(Response {
            status,
            body,
            link: None,
        })
    }
}

/// Reads an environment variable.
pub type EnvSource = fn(&str) -> Option<String>;
/// Runs `gh` with these arguments: its output, if it succeeded.
pub type GhSource = fn(&[&str]) -> Option<String>;

pub struct GitHub {
    host: String,
    api: String,
    graphql: String,
    http: Box<dyn Http>,
    token: OnceLock<Option<String>>,
    protocol: OnceLock<Protocol>,
    /// Where the token and protocol come from: the environment and the
    /// `gh` CLI, swappable for tests.
    env: EnvSource,
    gh: GhSource,
}

impl GitHub {
    pub fn new(host: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // a renamed repo's page redirects within the API: the token
            // goes along, or a private one would look gone
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .build()
            .into();
        let mut g = Self::with_http(host, Box::new(Ureq(agent)));
        // for tests of the programs: a stand-in for the API
        if let Some(base) = std::env::var("ARGUS_GITHUB_API")
            .ok()
            .filter(|b| !b.is_empty())
        {
            g.graphql = format!("{base}/graphql");
            g.api = base;
        }
        g
    }

    pub fn with_http(host: &str, http: Box<dyn Http>) -> Self {
        let api = if host.eq_ignore_ascii_case("github.com") {
            "https://api.github.com".to_string()
        } else {
            format!("https://{host}/api/v3")
        };
        let graphql = match host.eq_ignore_ascii_case("github.com") {
            true => "https://api.github.com/graphql".to_string(),
            false => format!("https://{host}/api/graphql"),
        };
        Self {
            host: host.to_string(),
            api,
            graphql,
            http,
            token: OnceLock::new(),
            protocol: OnceLock::new(),
            env: |v| std::env::var(v).ok(),
            gh: |args| run_cli("gh", args),
        }
    }

    /// Read environment variables and run `gh` through these instead.
    pub fn with_sources(self, env: EnvSource, gh: GhSource) -> Self {
        Self { env, gh, ..self }
    }

    /// Pre-seed the token (tests; or callers with their own credential source).
    pub fn with_token(self, token: Option<String>) -> Self {
        let _ = self.token.set(token);
        self
    }

    pub fn with_protocol(self, p: Protocol) -> Self {
        let _ = self.protocol.set(p);
        self
    }

    fn token(&self) -> Option<&str> {
        self.token
            .get_or_init(|| {
                let env = ["GH_TOKEN", "GITHUB_TOKEN"]
                    .iter()
                    .find_map(|v| (self.env)(v));
                env.filter(|t| !t.is_empty())
                    .or_else(|| (self.gh)(&["auth", "token", "--hostname", &self.host]))
            })
            .as_deref()
    }

    fn get_all(&self, path: &str) -> Result<Vec<ApiRepo>, ForgeError> {
        let mut url = Some(format!("{}{path}", self.api));
        let mut out = vec![];
        let mut pages = 0;
        while let Some(u) = url.take() {
            pages += 1;
            if pages > 200 {
                return Err(ForgeError::Other("too many pages".into()));
            }
            let r = self.http.get(&u, self.token()).map_err(ForgeError::Other)?;
            match r.status {
                200 => {}
                401 | 403 => return Err(ForgeError::Auth(api_message(&r.body, r.status))),
                404 => return Err(ForgeError::NotFound(path.to_string())),
                s => return Err(ForgeError::Other(api_message(&r.body, s))),
            }
            let page: Vec<ApiRepo> = serde_json::from_str(&r.body)
                .map_err(|e| ForgeError::Other(format!("decoding {u}: {e}")))?;
            out.extend(page);
            url = r.link.as_deref().and_then(next_link);
        }
        Ok(out)
    }

    fn login(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct User {
            login: String,
        }
        let r = self
            .http
            .get(&format!("{}/user", self.api), self.token())
            .ok()?;
        (r.status == 200)
            .then(|| serde_json::from_str::<User>(&r.body).ok().map(|u| u.login))
            .flatten()
    }

    fn convert(&self, repos: Vec<ApiRepo>) -> Vec<RemoteRepo> {
        let mut v: Vec<RemoteRepo> = repos
            .into_iter()
            .map(|r| RemoteRepo {
                id: RepoId::new(&self.host, r.owner.login, r.name),
                ssh_url: r.ssh_url,
                https_url: r.clone_url,
                archived: r.archived,
                fork: r.fork,
                default_branch: r.default_branch,
                description: r.description,
            })
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v.dedup_by(|a, b| a.id == b.id);
        v
    }
}

impl Forge for GitHub {
    fn kind(&self) -> &str {
        "github"
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn list(&self, owner: &str, team: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError> {
        let repos = if let Some(team) = team {
            self.get_all(&format!("/orgs/{owner}/teams/{team}/repos?per_page=100"))?
        } else {
            match self.get_all(&format!("/orgs/{owner}/repos?per_page=100&type=all")) {
                Ok(r) => r,
                Err(ForgeError::NotFound(_)) => {
                    // A user, not an org. Our own account also lists private repos.
                    if self.login().is_some_and(|l| l.eq_ignore_ascii_case(owner)) {
                        self.get_all("/user/repos?per_page=100&affiliation=owner&visibility=all")?
                    } else {
                        self.get_all(&format!("/users/{owner}/repos?per_page=100&type=owner"))?
                    }
                }
                Err(e) => return Err(e),
            }
        };
        // Team and user listings may contain repos of other owners.
        Ok(self
            .convert(repos)
            .into_iter()
            .filter(|r| r.id.owner.eq_ignore_ascii_case(owner))
            .collect())
    }

    /// The repo's own page in the API: GitHub sends a renamed or moved
    /// repo's on to what it is now.
    fn whereabouts(&self, id: &RepoId) -> Result<Whereabouts, ForgeError> {
        let mut url = format!("{}/repos/{}/{}", self.api, id.owner, id.name);
        for _ in 0..2 {
            let r = self
                .http
                .get(&url, self.token())
                .map_err(ForgeError::Other)?;
            match r.status {
                200 => {
                    let now: ApiRepo = serde_json::from_str(&r.body)
                        .map_err(|e| ForgeError::Other(format!("decoding {url}: {e}")))?;
                    let same = now.owner.login.eq_ignore_ascii_case(&id.owner)
                        && now.name.eq_ignore_ascii_case(&id.name);
                    return Ok(match same {
                        true => Whereabouts::Here,
                        false => {
                            Whereabouts::Moved(RepoId::new(&self.host, now.owner.login, now.name))
                        }
                    });
                }
                // not followed by the client: where it says, once
                301 | 302 | 307 | 308 => {
                    #[derive(Deserialize)]
                    struct Moved {
                        url: String,
                    }
                    let to: Moved = serde_json::from_str(&r.body)
                        .map_err(|_| ForgeError::Other(api_message(&r.body, r.status)))?;
                    url = to.url;
                }
                404 => return Ok(Whereabouts::Gone),
                401 | 403 => return Err(ForgeError::Auth(api_message(&r.body, r.status))),
                s => return Err(ForgeError::Other(api_message(&r.body, s))),
            }
        }
        Err(ForgeError::Other(format!("{url}: redirected again")))
    }

    /// One GraphQL query: the counts, and for each branch the newest pull
    /// requests with that head name, here and in the repo this one is a
    /// fork of. Others' branches can share a name, so only those from this
    /// repo's owner count.
    fn info(
        &self,
        id: &RepoId,
        branches: &[String],
        owners: &[String],
    ) -> Result<Option<RepoInfo>, ForgeError> {
        let Some(token) = self.token() else {
            return Err(ForgeError::Auth(
                "pull requests and issues need a token: gh auth login, or set GH_TOKEN".into(),
            ));
        };
        let branches = &branches[..branches.len().min(MAX_BRANCHES)];
        let body = serde_json::json!({
            "query": info_query(branches.len()),
            "variables": info_variables(id, branches),
        })
        .to_string();
        let url = &self.graphql;
        let r = self
            .http
            .post(url, Some(token), &body)
            .map_err(ForgeError::Other)?;
        match r.status {
            200 => {}
            401 | 403 => return Err(ForgeError::Auth(api_message(&r.body, r.status))),
            s => return Err(ForgeError::Other(api_message(&r.body, s))),
        }
        let answer: GraphQl = serde_json::from_str(&r.body)
            .map_err(|e| ForgeError::Other(format!("decoding {url}: {e}")))?;
        let Some(repo) = answer.data.and_then(|d| d.repository) else {
            return Err(match answer.errors.into_iter().next() {
                Some(e) if e.kind.as_deref() == Some("NOT_FOUND") => {
                    ForgeError::NotFound(format!("{}/{}", id.owner, id.name))
                }
                Some(e) => ForgeError::Other(e.message),
                None => ForgeError::Other("no answer".into()),
            });
        };
        let ours = |n: &&ApiPull| {
            n.head_repository_owner.as_ref().is_some_and(|o| {
                o.login.eq_ignore_ascii_case(&id.owner)
                    || owners.iter().any(|w| o.login.eq_ignore_ascii_case(w))
            })
        };
        let pulls = branches
            .iter()
            .enumerate()
            .filter_map(|(i, branch)| {
                let key = format!("b{i}");
                let here = repo.branches.get(&key).into_iter().flat_map(|c| &c.nodes);
                let parent = repo.parent.as_ref();
                let there = parent
                    .and_then(|p| p.branches.get(&key))
                    .into_iter()
                    .flat_map(|c| &c.nodes);
                let all: Vec<(&ApiPull, Option<&str>)> = here
                    .map(|n| (n, None))
                    .chain(there.map(|n| (n, parent.map(|p| p.name_with_owner.as_str()))))
                    .filter(|(n, _)| ours(n))
                    .collect();
                let (n, into) = all
                    .iter()
                    .find(|(n, _)| n.state == "OPEN")
                    .or(all.first())?;
                Some(n.convert(branch, *into))
            })
            .collect();
        Ok(Some(RepoInfo {
            open_issues: repo.issues.total_count,
            open_pulls: repo.pull_requests.total_count,
            parent: repo.parent.as_ref().map(|p| ParentInfo {
                name: p.name_with_owner.clone(),
                open_issues: p.issues.as_ref().map_or(0, |c| c.total_count),
                open_pulls: p.pull_requests.as_ref().map_or(0, |c| c.total_count),
            }),
            pulls,
        }))
    }

    fn protocol(&self) -> Protocol {
        *self.protocol.get_or_init(|| {
            match (self.gh)(&["config", "get", "git_protocol", "--host", &self.host]).as_deref() {
                Some("ssh") => Protocol::Ssh,
                _ => Protocol::Https,
            }
        })
    }
}

#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}

/// How many branches one [`Forge::info`] asks about: each adds to the
/// query's cost, and a repo with more has mostly forgotten ones.
const MAX_BRANCHES: usize = 20;

/// The query for `n` branches, named by the variables `$b0`, `$b1`, ….
fn info_query(n: usize) -> String {
    let vars: String = (0..n).map(|i| format!(", $b{i}: String!")).collect();
    let pulls: String = (0..n)
        .map(|i| {
            format!(
                "b{i}: pullRequests(headRefName: $b{i}, first: 5, \
                 orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ ...pr }} }} "
            )
        })
        .collect();
    // (a fragment nothing uses is an error)
    let fragment = match n {
        0 => "",
        _ => {
            " fragment pr on PullRequest { number title state isDraft url reviewDecision \
             headRepositoryOwner { login } \
             commits(last: 1) { nodes { commit { statusCheckRollup { state } } } } }"
        }
    };
    format!(
        "query($owner: String!, $name: String!{vars}) {{ \
           repository(owner: $owner, name: $name) {{ \
             issues(states: OPEN) {{ totalCount }} \
             pullRequests(states: OPEN) {{ totalCount }} \
             {pulls}\
             parent {{ nameWithOwner \
               issues(states: OPEN) {{ totalCount }} \
               pullRequests(states: OPEN) {{ totalCount }} \
               {pulls}}} }} }}{fragment}"
    )
}

fn info_variables(id: &RepoId, branches: &[String]) -> serde_json::Value {
    let mut v = serde_json::json!({"owner": id.owner, "name": id.name});
    for (i, b) in branches.iter().enumerate() {
        v[format!("b{i}")] = b.as_str().into();
    }
    v
}

#[derive(Deserialize)]
struct GraphQl {
    data: Option<GraphQlData>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlData {
    repository: Option<ApiInfo>,
}

#[derive(Deserialize)]
struct GraphQlError {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: String,
}

#[derive(Deserialize)]
struct Count {
    #[serde(rename = "totalCount")]
    total_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiInfo {
    issues: Count,
    pull_requests: Count,
    parent: Option<ApiParent>,
    /// `b0`, `b1`, …: the pull requests with each branch's name.
    #[serde(flatten)]
    branches: std::collections::HashMap<String, Nodes<ApiPull>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiParent {
    name_with_owner: String,
    issues: Option<Count>,
    pull_requests: Option<Count>,
    #[serde(flatten)]
    branches: std::collections::HashMap<String, Nodes<ApiPull>>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPull {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    url: String,
    review_decision: Option<String>,
    head_repository_owner: Option<ApiOwner>,
    commits: Option<Nodes<ApiPullCommit>>,
}

#[derive(Deserialize)]
struct ApiPullCommit {
    commit: ApiCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiCommit {
    status_check_rollup: Option<ApiRollup>,
}

#[derive(Deserialize)]
struct ApiRollup {
    state: String,
}

impl ApiPull {
    fn convert(&self, branch: &str, into: Option<&str>) -> PullRequest {
        let rollup = self
            .commits
            .iter()
            .flat_map(|c| &c.nodes)
            .find_map(|c| c.commit.status_check_rollup.as_ref());
        PullRequest {
            branch: branch.to_string(),
            number: self.number,
            title: self.title.clone(),
            state: match self.state.as_str() {
                "MERGED" => PullState::Merged,
                "CLOSED" => PullState::Closed,
                _ => PullState::Open,
            },
            draft: self.is_draft,
            url: self.url.clone(),
            review: self.review_decision.as_deref().and_then(|r| match r {
                "APPROVED" => Some(Review::Approved),
                "CHANGES_REQUESTED" => Some(Review::ChangesRequested),
                "REVIEW_REQUIRED" => Some(Review::Required),
                _ => None,
            }),
            checks: rollup.and_then(|r| match r.state.as_str() {
                "SUCCESS" => Some(Checks::Passing),
                "FAILURE" | "ERROR" => Some(Checks::Failing),
                "PENDING" | "EXPECTED" => Some(Checks::Pending),
                _ => None,
            }),
            into: into.map(String::from),
        }
    }
}

#[derive(Deserialize)]
struct ApiRepo {
    name: String,
    owner: ApiOwner,
    ssh_url: String,
    clone_url: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    fork: bool,
    default_branch: Option<String>,
    description: Option<String>,
}

/// `program`'s trimmed output, if it succeeded and printed something.
fn run_cli(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

fn api_message(body: &str, status: u16) -> String {
    #[derive(Deserialize)]
    struct E {
        message: String,
    }
    match serde_json::from_str::<E>(body) {
        Ok(e) => format!("HTTP {status}: {}", e.message),
        Err(_) => format!("HTTP {status}"),
    }
}

/// Extract the `rel="next"` URL from a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let mut it = part.split(';');
        let url = it.next()?.trim();
        let is_next = it.any(|p| {
            let p = p.trim().replace(' ', "");
            p == "rel=\"next\"" || p == "rel=next"
        });
        is_next.then(|| {
            url.trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        routes: HashMap<String, (u16, String, Option<String>)>,
        seen: Mutex<Vec<(String, Option<String>)>>,
        /// What a POST to a URL answers, and the bodies posted.
        posts: HashMap<String, (u16, String)>,
        posted: Mutex<Vec<(String, String)>>,
    }

    impl Fake {
        fn route(mut self, url: &str, status: u16, body: &str, link: Option<&str>) -> Self {
            self.routes
                .insert(url.into(), (status, body.into(), link.map(String::from)));
            self
        }
    }

    impl Http for std::sync::Arc<Fake> {
        fn get(&self, url: &str, token: Option<&str>) -> Result<Response, String> {
            self.seen
                .lock()
                .unwrap()
                .push((url.into(), token.map(String::from)));
            let (status, body, link) = self.routes.get(url).cloned().unwrap_or((
                404,
                r#"{"message":"Not Found"}"#.into(),
                None,
            ));
            Ok(Response { status, body, link })
        }

        fn post(&self, url: &str, token: Option<&str>, body: &str) -> Result<Response, String> {
            assert_eq!(token, Some("tok"));
            self.posted.lock().unwrap().push((url.into(), body.into()));
            let (status, body) = self.posts.get(url).cloned().ok_or("unreachable")?;
            Ok(Response {
                status,
                body,
                link: None,
            })
        }
    }

    fn graphql(url: &str, status: u16, body: &str) -> (GitHub, std::sync::Arc<Fake>) {
        let mut f = Fake::default();
        f.posts.insert(url.into(), (status, body.into()));
        let host = match url.contains("api.github.com") {
            true => "github.com",
            false => "ghe.corp",
        };
        let f = std::sync::Arc::new(f);
        (
            GitHub::with_http(host, Box::new(f.clone())).with_token(Some("tok".into())),
            f,
        )
    }

    fn node(n: u64, state: &str, owner: &str, rest: &str) -> String {
        format!(
            r#"{{"number":{n},"title":"PR {n}","state":"{state}",
                "url":"https://github.com/x/pull/{n}","headRepositoryOwner":{{"login":"{owner}"}}{rest}}}"#
        )
    }

    const GQL: &str = "https://api.github.com/graphql";

    #[test]
    fn info_is_one_query_for_counts_and_each_branchs_pull_request() {
        let body = format!(
            r#"{{"data":{{"repository":{{
                "issues":{{"totalCount":7}},"pullRequests":{{"totalCount":3}},
                "b0":{{"nodes":[{}, {}, {}]}},
                "b1":{{"nodes":[]}},
                "b2":{{"nodes":[{}]}},
                "parent":null}}}}}}"#,
            // someone else's branch of the same name: not ours
            node(9, "OPEN", "stranger", ""),
            // ours: an older open one wins over a newer closed one
            node(8, "CLOSED", "Me", ""),
            node(
                5,
                "OPEN",
                "me",
                r#","reviewDecision":"CHANGES_REQUESTED","commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}"#
            ),
            node(
                4,
                "MERGED",
                "me",
                r#","reviewDecision":null,"commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}"#
            ),
        );
        let (g, f) = graphql(GQL, 200, &body);
        let id = RepoId::new("github.com", "me", "w");
        let branches = ["feat".to_string(), "none".into(), "done".into()];
        let info = g.info(&id, &branches, &[]).unwrap().unwrap();
        assert_eq!((info.open_issues, info.open_pulls), (7, 3));
        assert_eq!(info.pulls.len(), 2, "{:?}", info.pulls);
        let p = info.pull("feat").unwrap();
        assert_eq!((p.number, p.state), (5, PullState::Open));
        assert_eq!(p.title, "PR 5");
        assert_eq!(p.url, "https://github.com/x/pull/5");
        assert_eq!(
            (p.review, p.checks, p.into.clone()),
            (Some(Review::ChangesRequested), Some(Checks::Failing), None)
        );
        let p = info.pull("done").unwrap();
        assert_eq!((p.number, p.state), (4, PullState::Merged));
        assert_eq!((p.review, p.checks), (None, None));
        assert!(info.pull("none").is_none());
        // one request, the names sent as variables, not spliced into the query
        let posted = f.posted.lock().unwrap();
        assert_eq!(posted.len(), 1);
        let sent: serde_json::Value = serde_json::from_str(&posted[0].1).unwrap();
        assert_eq!(sent["variables"]["owner"], "me");
        assert_eq!(sent["variables"]["name"], "w");
        assert_eq!(sent["variables"]["b0"], "feat");
        assert_eq!(sent["variables"]["b2"], "done");
        let q = sent["query"].as_str().unwrap();
        assert!(!q.contains("feat") && q.contains("headRefName: $b2"), "{q}");
    }

    #[test]
    fn info_looks_in_the_repo_a_fork_came_from_too() {
        let body = format!(
            r#"{{"data":{{"repository":{{
                "issues":{{"totalCount":0}},"pullRequests":{{"totalCount":0}},
                "b0":{{"nodes":[]}},
                "parent":{{"nameWithOwner":"up/w","b0":{{"nodes":[{}, {}]}}}}}}}}}}"#,
            node(30, "OPEN", "other", ""),
            node(
                21,
                "OPEN",
                "me",
                r#","isDraft":true,"reviewDecision":"REVIEW_REQUIRED","commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"PENDING"}}}]}"#
            ),
        );
        let (g, _) = graphql(GQL, 200, &body);
        let id = RepoId::new("github.com", "me", "w");
        let info = g.info(&id, &["feat".to_string()], &[]).unwrap().unwrap();
        let p = info.pull("feat").unwrap();
        assert_eq!((p.number, p.into.as_deref()), (21, Some("up/w")));
        // the parent's counts come along (none given: none)
        assert_eq!(
            info.parent,
            Some(ParentInfo {
                name: "up/w".into(),
                open_issues: 0,
                open_pulls: 0
            })
        );
        let counted = r#"{"data":{"repository":{"issues":{"totalCount":0},"pullRequests":{"totalCount":1},
            "parent":{"nameWithOwner":"up/w","issues":{"totalCount":26},"pullRequests":{"totalCount":7}}}}}"#;
        let (g, f) = graphql(GQL, 200, counted);
        let info = g.info(&id, &[], &[]).unwrap().unwrap();
        let parent = info.parent.clone().unwrap();
        assert_eq!((parent.open_issues, parent.open_pulls), (26, 7));
        assert_eq!(
            info.counts(),
            "up/w: 26 open issues, 7 open pull requests; this fork: 0 open issues, 1 open pull request"
        );
        let sent = f.posted.lock().unwrap()[0].1.clone();
        assert!(
            sent.contains("parent { nameWithOwner issues(states: OPEN)"),
            "{sent}"
        );
        // with the clone's other remotes' owners, their branches count too:
        // a fork pushed to beside the upstream this was cloned from
        let (g, _) = graphql(GQL, 200, &body);
        let up = RepoId::new("github.com", "up", "w");
        let none = g.info(&up, &["feat".to_string()], &[]).unwrap().unwrap();
        assert!(none.pulls.is_empty(), "not the upstream owner's own");
        let (g, _) = graphql(GQL, 200, &body);
        let mine = g
            .info(&up, &["feat".to_string()], &["ME".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(mine.pull("feat").map(|p| p.number), Some(21));
        assert_eq!(
            (p.draft, p.review, p.checks),
            (true, Some(Review::Required), Some(Checks::Pending))
        );
    }

    #[test]
    fn info_maps_every_review_and_check_state() {
        let one = |rest: &str| {
            let body = format!(
                r#"{{"data":{{"repository":{{"issues":{{"totalCount":0}},"pullRequests":{{"totalCount":0}},
                    "b0":{{"nodes":[{}]}},"parent":null}}}}}}"#,
                node(1, "OPEN", "me", rest)
            );
            let (g, _) = graphql(GQL, 200, &body);
            let id = RepoId::new("github.com", "me", "w");
            let p = g.info(&id, &["f".to_string()], &[]).unwrap().unwrap().pulls[0].clone();
            (p.review, p.checks)
        };
        let checks = |state: &str| {
            one(&format!(
                r#","commits":{{"nodes":[{{"commit":{{"statusCheckRollup":{{"state":"{state}"}}}}}}]}}"#
            ))
            .1
        };
        assert_eq!(checks("SUCCESS"), Some(Checks::Passing));
        assert_eq!(checks("FAILURE"), Some(Checks::Failing));
        assert_eq!(checks("ERROR"), Some(Checks::Failing));
        assert_eq!(checks("PENDING"), Some(Checks::Pending));
        assert_eq!(checks("EXPECTED"), Some(Checks::Pending));
        assert_eq!(checks("SOMETHING_NEW"), None);
        assert_eq!(
            one(r#","reviewDecision":"APPROVED""#).0,
            Some(Review::Approved)
        );
        assert_eq!(one(r#","reviewDecision":"NEW""#).0, None);
        assert_eq!(one(""), (None, None));
        // and how it ended
        let state = |state: &str| {
            let body = format!(
                r#"{{"data":{{"repository":{{"issues":{{"totalCount":0}},"pullRequests":{{"totalCount":0}},
                    "b0":{{"nodes":[{}]}},"parent":null}}}}}}"#,
                node(1, state, "me", "")
            );
            let (g, _) = graphql(GQL, 200, &body);
            let id = RepoId::new("github.com", "me", "w");
            g.info(&id, &["f".to_string()], &[]).unwrap().unwrap().pulls[0].state
        };
        assert_eq!(state("OPEN"), PullState::Open);
        assert_eq!(state("CLOSED"), PullState::Closed);
        assert_eq!(state("MERGED"), PullState::Merged);
    }

    #[test]
    fn info_errors_are_classified_and_enterprise_has_its_own_endpoint() {
        let id = RepoId::new("github.com", "me", "w");
        let (g, _) = graphql(GQL, 401, r#"{"message":"Bad credentials"}"#);
        assert!(
            matches!(g.info(&id, &[], &[]), Err(ForgeError::Auth(m)) if m.contains("Bad credentials"))
        );
        let gone = r#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a Repository"}]}"#;
        let (g, _) = graphql(GQL, 200, gone);
        assert!(matches!(g.info(&id, &[], &[]), Err(ForgeError::NotFound(m)) if m == "me/w"));
        let other = r#"{"errors":[{"message":"Something went wrong"}]}"#;
        let (g, _) = graphql(GQL, 200, other);
        assert!(
            matches!(g.info(&id, &[], &[]), Err(ForgeError::Other(m)) if m == "Something went wrong")
        );
        let (g, _) = graphql(GQL, 502, "<html>");
        assert!(matches!(g.info(&id, &[], &[]), Err(ForgeError::Other(m)) if m == "HTTP 502"));
        let (g, _) = graphql(GQL, 200, "not json");
        assert!(matches!(g.info(&id, &[], &[]), Err(ForgeError::Other(_))));
        // without a token there's nothing to ask with, and nothing is sent
        let f = std::sync::Arc::new(Fake::default());
        let g = GitHub::with_http("github.com", Box::new(f.clone())).with_token(None);
        assert!(
            matches!(g.info(&id, &[], &[]), Err(ForgeError::Auth(m)) if m.contains("gh auth login"))
        );
        assert!(f.posted.lock().unwrap().is_empty());
        // enterprise
        let ok = r#"{"data":{"repository":{"issues":{"totalCount":1},"pullRequests":{"totalCount":2},"parent":null}}}"#;
        let (g, f) = graphql("https://ghe.corp/api/graphql", 200, ok);
        let info = g
            .info(&RepoId::new("ghe.corp", "o", "n"), &[], &[])
            .unwrap()
            .unwrap();
        assert_eq!((info.open_issues, info.open_pulls), (1, 2));
        assert_eq!(
            f.posted.lock().unwrap()[0].0,
            "https://ghe.corp/api/graphql"
        );
    }

    #[test]
    fn info_asks_about_at_most_twenty_branches() {
        let ok = r#"{"data":{"repository":{"issues":{"totalCount":0},"pullRequests":{"totalCount":0},"parent":null}}}"#;
        let (g, f) = graphql(GQL, 200, ok);
        let branches: Vec<String> = (0..30).map(|i| format!("br{i}")).collect();
        g.info(&RepoId::new("github.com", "me", "w"), &branches, &[])
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&f.posted.lock().unwrap()[0].1).unwrap();
        assert_eq!(sent["variables"]["b19"], "br19");
        assert!(sent["variables"].get("b20").is_none());
        assert!(sent["query"].as_str().unwrap().contains("fragment pr on"));
        // with no branch to ask about, no fragment: unused, it's refused
        g.info(&RepoId::new("github.com", "me", "w"), &[], &[])
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&f.posted.lock().unwrap()[1].1).unwrap();
        let q = sent["query"].as_str().unwrap();
        assert!(!q.contains("fragment") && !q.contains("...pr"), "{q}");
    }

    #[test]
    fn whereabouts_tells_renamed_from_gone() {
        let id = RepoId::new("github.com", "acme", "old");
        let at = format!("{API}/repos/acme/old");
        let ask = |f: Fake| gh_with(f).0.whereabouts(&id);
        // still there, whatever the capitals
        let f = Fake::default().route(&at, 200, &repo("Acme", "Old", false), None);
        assert_eq!(ask(f).unwrap(), Whereabouts::Here);
        // followed to its new name by the HTTP client
        let f = Fake::default().route(&at, 200, &repo("other", "fresh", false), None);
        assert_eq!(
            ask(f).unwrap(),
            Whereabouts::Moved(RepoId::new("github.com", "other", "fresh"))
        );
        // or not followed: the answer says where to look, once
        let moved =
            r#"{"message":"Moved Permanently","url":"https://api.github.com/repositories/42"}"#;
        let f = Fake::default().route(&at, 301, moved, None).route(
            "https://api.github.com/repositories/42",
            200,
            &repo("other", "fresh", false),
            None,
        );
        let (g, f) = gh_with(f);
        assert_eq!(
            g.whereabouts(&id).unwrap(),
            Whereabouts::Moved(RepoId::new("github.com", "other", "fresh"))
        );
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].1.as_deref(), Some("tok"), "the token goes along");
        drop(seen);
        // a redirect to nowhere isn't chased
        let f = Fake::default().route(&at, 301, moved, None).route(
            "https://api.github.com/repositories/42",
            301,
            moved,
            None,
        );
        assert!(matches!(ask(f), Err(ForgeError::Other(_))));
        // gone, or not ours to see
        assert_eq!(ask(Fake::default()).unwrap(), Whereabouts::Gone);
        // anything else is an error, not a guess
        let f = Fake::default().route(&at, 401, r#"{"message":"Bad credentials"}"#, None);
        assert!(matches!(ask(f), Err(ForgeError::Auth(m)) if m.contains("Bad credentials")));
        let f = Fake::default().route(&at, 500, "", None);
        assert!(matches!(ask(f), Err(ForgeError::Other(m)) if m == "HTTP 500"));
        let f = Fake::default().route(&at, 200, "not json", None);
        assert!(matches!(ask(f), Err(ForgeError::Other(_))));
    }

    fn repo(owner: &str, name: &str, archived: bool) -> String {
        format!(
            r#"{{"name":"{name}","owner":{{"login":"{owner}"}},"ssh_url":"git@github.com:{owner}/{name}.git",
               "clone_url":"https://github.com/{owner}/{name}.git","archived":{archived},"fork":false,
               "default_branch":"main","description":null,"extra_field":1}}"#
        )
    }

    fn gh_with(f: Fake) -> (GitHub, std::sync::Arc<Fake>) {
        let f = std::sync::Arc::new(f);
        (
            GitHub::with_http("github.com", Box::new(f.clone())).with_token(Some("tok".into())),
            f,
        )
    }

    const API: &str = "https://api.github.com";

    #[test]
    fn lists_org_with_pagination() {
        let p1 = format!("{API}/orgs/acme/repos?per_page=100&type=all");
        let p2 = format!("{API}/orgs/acme/repos?per_page=100&type=all&page=2");
        let f = Fake::default()
            .route(
                &p1,
                200,
                &format!("[{},{}]", repo("acme", "b", false), repo("acme", "a", true)),
                Some(&format!("<{p2}>; rel=\"next\", <{p2}>; rel=\"last\"")),
            )
            .route(
                &p2,
                200,
                &format!("[{}]", repo("acme", "c", false)),
                Some(&format!("<{p1}>; rel=\"prev\"")),
            );
        let (g, f) = gh_with(f);
        let r = g.list("acme", None).unwrap();
        assert_eq!(
            r.iter().map(|r| r.id.to_string()).collect::<Vec<_>>(),
            vec![
                "github.com/acme/a",
                "github.com/acme/b",
                "github.com/acme/c"
            ]
        );
        assert!(r[0].archived && !r[1].archived);
        assert_eq!(r[0].https_url, "https://github.com/acme/a.git");
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|(_, t)| t.as_deref() == Some("tok")));
    }

    #[test]
    fn falls_back_to_user_and_own_account() {
        let other = Fake::default().route(
            &format!("{API}/users/bob/repos?per_page=100&type=owner"),
            200,
            &format!("[{}]", repo("bob", "x", false)),
            None,
        );
        let (g, _) = gh_with(other);
        assert_eq!(g.list("bob", None).unwrap().len(), 1);

        let me = Fake::default()
            .route(&format!("{API}/user"), 200, r#"{"login":"Me"}"#, None)
            .route(
                &format!("{API}/user/repos?per_page=100&affiliation=owner&visibility=all"),
                200,
                &format!(
                    "[{},{}]",
                    repo("me", "private", false),
                    repo("someorg", "nope", false)
                ),
                None,
            );
        let (g, _) = gh_with(me);
        let r = g.list("me", None).unwrap();
        assert_eq!(r.len(), 1, "repos of other owners are filtered out");
        assert_eq!(r[0].id.name, "private");
    }

    #[test]
    fn team_listing() {
        let f = Fake::default().route(
            &format!("{API}/orgs/acme/teams/core/repos?per_page=100"),
            200,
            &format!("[{}]", repo("acme", "t", false)),
            None,
        );
        let (g, _) = gh_with(f);
        assert_eq!(g.list("acme", Some("core")).unwrap()[0].id.name, "t");
    }

    #[test]
    fn errors_are_classified() {
        let f = Fake::default().route(
            &format!("{API}/orgs/acme/repos?per_page=100&type=all"),
            401,
            r#"{"message":"Bad credentials"}"#,
            None,
        );
        let (g, _) = gh_with(f);
        match g.list("acme", None) {
            Err(ForgeError::Auth(m)) => assert!(m.contains("Bad credentials")),
            other => panic!("{other:?}"),
        }
        let f = Fake::default().route(
            &format!("{API}/orgs/acme/repos?per_page=100&type=all"),
            200,
            "not json",
            None,
        );
        let (g, _) = gh_with(f);
        assert!(matches!(g.list("acme", None), Err(ForgeError::Other(_))));
        let (g, _) = gh_with(Fake::default());
        assert!(matches!(
            g.list("nobody", None),
            Err(ForgeError::NotFound(_))
        ));
    }

    #[test]
    fn enterprise_api_base() {
        let f = Fake::default().route(
            "https://ghe.corp/api/v3/orgs/x/repos?per_page=100&type=all",
            200,
            "[]",
            None,
        );
        let f = std::sync::Arc::new(f);
        let g = GitHub::with_http("ghe.corp", Box::new(f.clone())).with_token(None);
        assert!(g.list("x", None).unwrap().is_empty());
        assert_eq!(
            f.seen.lock().unwrap()[0].1,
            None,
            "no token, no auth header"
        );
    }

    #[test]
    fn link_header_parsing() {
        assert_eq!(
            next_link(r#"<https://a/2>; rel="next", <https://a/9>; rel="last""#).as_deref(),
            Some("https://a/2")
        );
        assert_eq!(
            next_link(r#"<https://a/1>; rel="prev", <https://a/9>; rel="last""#),
            None
        );
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn explicit_protocol() {
        let (g, _) = gh_with(Fake::default());
        assert_eq!(g.with_protocol(Protocol::Ssh).protocol(), Protocol::Ssh);
    }

    fn sourced(env: EnvSource, gh: GhSource) -> GitHub {
        GitHub::with_http("github.com", Box::new(std::sync::Arc::new(Fake::default())))
            .with_sources(env, gh)
    }

    #[test]
    fn the_token_comes_from_gh_token_github_token_then_gh() {
        let gh: GhSource = |args| {
            assert_eq!(args, ["auth", "token", "--hostname", "github.com"]);
            Some("from-gh".into())
        };
        let cases: [(EnvSource, &str); 4] = [
            (|v| (v == "GH_TOKEN").then(|| "a".into()), "a"),
            (|v| (v == "GITHUB_TOKEN").then(|| "b".into()), "b"),
            // set but empty: as if unset
            (|v| (v == "GH_TOKEN").then(String::new), "from-gh"),
            (|_| None, "from-gh"),
        ];
        for (env, want) in cases {
            assert_eq!(sourced(env, gh).token(), Some(want));
        }
    }

    #[test]
    fn the_protocol_follows_gh_config() {
        let ssh: GhSource = |args| {
            assert_eq!(
                args,
                ["config", "get", "git_protocol", "--host", "github.com"]
            );
            Some("ssh".into())
        };
        assert_eq!(sourced(|_| None, ssh).protocol(), Protocol::Ssh);
        assert_eq!(
            sourced(|_| None, |_| Some("https".into())).protocol(),
            Protocol::Https
        );
        assert_eq!(sourced(|_| None, |_| None).protocol(), Protocol::Https);
    }

    #[test]
    fn kind_and_host() {
        let (g, _) = gh_with(Fake::default());
        assert_eq!((g.kind(), g.host()), ("github", "github.com"));
        let e = GitHub::with_http(
            "ghe.example.com",
            Box::new(std::sync::Arc::new(Fake::default())),
        );
        assert_eq!(e.host(), "ghe.example.com");
    }

    #[test]
    fn paging_stops_after_200_pages() {
        let p = format!("{API}/orgs/acme/repos?per_page=100&type=all");
        let f = Fake::default().route(&p, 200, "[]", Some(&format!("<{p}>; rel=\"next\"")));
        let (g, f) = gh_with(f);
        match g.list("acme", None) {
            Err(ForgeError::Other(m)) => assert_eq!(m, "too many pages"),
            other => panic!("{other:?}"),
        }
        assert_eq!(f.seen.lock().unwrap().len(), 200);
    }

    #[test]
    fn a_cli_counts_only_when_it_succeeds_and_prints_something() {
        assert_eq!(run_cli("sh", &["-c", "echo ' hi '"]), Some("hi".into()));
        assert_eq!(run_cli("sh", &["-c", "echo hi; exit 1"]), None);
        assert_eq!(run_cli("sh", &["-c", "true"]), None);
        assert_eq!(run_cli("/no/such/program", &[]), None);
    }
}
