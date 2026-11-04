//! GitHub (and GitHub Enterprise) via the REST API.

use std::process::Command;
use std::sync::OnceLock;

use argus_core::forge::{Forge, ForgeError, Protocol, RemoteRepo};
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
}

pub struct GitHub {
    host: String,
    api: String,
    http: Box<dyn Http>,
    token: OnceLock<Option<String>>,
    protocol: OnceLock<Protocol>,
}

impl GitHub {
    pub fn new(host: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self::with_http(host, Box::new(Ureq(agent)))
    }

    pub fn with_http(host: &str, http: Box<dyn Http>) -> Self {
        let api = if host.eq_ignore_ascii_case("github.com") {
            "https://api.github.com".to_string()
        } else {
            format!("https://{host}/api/v3")
        };
        Self {
            host: host.to_string(),
            api,
            http,
            token: OnceLock::new(),
            protocol: OnceLock::new(),
        }
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
                    .find_map(|v| std::env::var(v).ok());
                env.filter(|t| !t.is_empty())
                    .or_else(|| gh(&["auth", "token", "--hostname", &self.host]))
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

    fn protocol(&self) -> Protocol {
        *self.protocol.get_or_init(|| {
            match gh(&["config", "get", "git_protocol", "--host", &self.host]).as_deref() {
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

fn gh(args: &[&str]) -> Option<String> {
    let out = Command::new("gh")
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
}
