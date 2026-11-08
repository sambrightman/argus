//! Benchmarks for the hot paths: discovery (with and without `-L` and its
//! optimizations), status and scanning real repos, a file's diff, config
//! loading with extended files, and rule matching. Fixtures are built once per
//! run in a temp directory.
//!
//! ```sh
//! cargo bench -p argus-core                          # all
//! cargo bench -p argus-core -- discover              # a group
//! cargo bench -p argus-core -- --save-baseline before   # then, after a change:
//! cargo bench -p argus-core -- --baseline before
//! ```

use std::hint::black_box;
use std::path::{Path, PathBuf};

use argus_core::config::Config;
use argus_core::diff::{Side, file_diff};
use argus_core::discover::{DiscoverOptions, Skip, Tuning, discover_all};
use argus_core::identity::RepoId;
use argus_core::rules::{Mark, RuleSet};
use argus_core::status::{StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use criterion::{Criterion, criterion_group, criterion_main};

/// `orgs` × `repos` fake repos (a `.git` directory each), with a few
/// subdirectories inside, plus non-repo directories (some named to be
/// skipped), and, with `links`, an alias per org, a chain of two links and
/// a loop back to the top in each org.
fn tree(root: &Path, orgs: usize, repos: usize, links: bool) {
    let mk = |p: PathBuf| std::fs::create_dir_all(p).unwrap();
    for o in 0..orgs {
        for r in 0..repos {
            let repo = root.join(format!("real/org{o}/proj{r}"));
            mk(repo.join(".git"));
            mk(repo.join("src/a/b"));
        }
        for m in 0..10 {
            mk(root.join(format!("real/org{o}/misc/m{m}/deep/x/y")));
            mk(root.join(format!("real/org{o}/misc/m{m}/node_modules/pkg/lib")));
        }
    }
    if !links {
        return;
    }
    let link = |target: &str, at: &str| {
        std::os::unix::fs::symlink(root.join(target), root.join(at)).unwrap()
    };
    mk(root.join("links/chain"));
    for o in 0..orgs {
        link(&format!("real/org{o}"), &format!("links/alias{o}"));
        link("real", &format!("real/org{o}/misc/up"));
        mk(root.join(format!("links/chain/c{o}")));
        link(&format!("real/org{o}"), &format!("links/chain/c{o}/to-org"));
    }
    link("links/chain", "links/hop");
}

fn discovery(c: &mut Criterion) {
    let t = tempfile::tempdir().unwrap();
    tree(t.path(), 20, 25, true);
    let roots = vec![t.path().to_path_buf()];
    let names: Vec<String> = ["node_modules", "target", "vendor", "build", "dist"]
        .map(String::from)
        .to_vec();
    let mut with_paths = names.clone();
    with_paths.extend([
        "go/pkg".to_string(),
        format!("{}/real/*/misc/m1", t.path().display()),
    ]);
    let opts = |skip: &[String], follow: bool, real: bool, prune: bool| DiscoverOptions {
        skip: Skip::new(skip, &roots).unwrap(),
        max_depth: 0,
        follow_symlinks: follow,
        tuning: Tuning {
            real_from_links: real,
            prune_covered_links: prune,
        },
        ..Default::default()
    };
    let mut g = c.benchmark_group("discover");
    for (name, o) in [
        ("plain", opts(&[], false, true, true)),
        ("name skips", opts(&names, false, true, true)),
        ("name and path skips", opts(&with_paths, false, true, true)),
        ("-L, no optimizations", opts(&names, true, false, false)),
        ("-L, real paths from links", opts(&names, true, true, false)),
        ("-L, pruning covered links", opts(&names, true, false, true)),
        ("-L, both", opts(&names, true, true, true)),
    ] {
        g.bench_function(name, |b| b.iter(|| black_box(discover_all(&roots, &o))));
    }
    g.finish();
}

/// `n` real git repos under `<sandbox>/dev`, a quarter of them with changes.
fn repos(sb: &Sandbox, n: usize) -> PathBuf {
    for i in 0..n {
        let r = sb.init(&format!("dev/r{i}"));
        if i % 4 == 0 {
            write(&r, "README", "changed\n");
            write(&r, "new", "untracked\n");
        }
    }
    sb.path("dev")
}

fn status_and_scan(c: &mut Criterion) {
    let sb = Sandbox::new();
    let dev = repos(&sb, 40);
    let one = dev.join("r0");
    let mut g = c.benchmark_group("status");
    g.bench_function("one repo", |b| {
        b.iter(|| black_box(status(&one, StatusOptions::default()).unwrap()))
    });
    let config = Config::parse(&format!("roots = ['{}']", dev.display())).unwrap();
    g.bench_function("scan 40 repos", |b| {
        b.iter(|| {
            let mut ws = Workspace::new(config.clone(), State::default()).unwrap();
            ws.scan();
            black_box(ws.entries.len())
        })
    });
    g.finish();
}

fn diff(c: &mut Criterion) {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let lines: String = (0..5000)
        .map(|i| format!("line {i} of the file\n"))
        .collect();
    write(&r, "big", &lines);
    commit(&r, "big");
    let changed: String = (0..5000)
        .map(|i| match i % 97 {
            0 => format!("changed {i}\n"),
            _ => format!("line {i} of the file\n"),
        })
        .collect();
    write(&r, "big", &changed);
    c.bench_function("diff 5000 lines, 52 changes", |b| {
        b.iter(|| black_box(file_diff(&r, "big", Side::Unstaged).unwrap()))
    });
}

fn config(c: &mut Criterion) {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    std::fs::write(
        d.join("base.toml"),
        "max_depth = 3\nfavourite = ['base/*']\n[[track]]\nowner = 'a'\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.join("conf.d")).unwrap();
    for i in 0..5 {
        std::fs::write(
            d.join(format!("conf.d/{i}.toml")),
            format!("extend_exclude_paths = ['s{i}']\n"),
        )
        .unwrap();
    }
    let main = d.join("config.toml");
    std::fs::write(
        &main,
        "extend = ['base.toml', 'conf.d/*.toml']\nextend_favourite = ['x/y']\n",
    )
    .unwrap();
    let mut g = c.benchmark_group("config");
    g.bench_function("load extending 6 files", |b| {
        b.iter(|| black_box(Config::load(&main).unwrap()))
    });
    g.bench_function("parse defaults", |b| {
        b.iter(|| black_box(Config::parse("").unwrap()))
    });
    g.finish();
}

fn rules(c: &mut Criterion) {
    let config = Config {
        favourite: (0..20).map(|i| format!("org{i}/*")).collect(),
        pull: (0..20).map(|i| format!("!org{i}/legacy-*")).collect(),
        ..Config::default()
    };
    let rs = RuleSet::new(&config).unwrap();
    let ids: Vec<RepoId> = (0..1000)
        .filter_map(|i| RepoId::from_url(&format!("git@github.com:org{}/repo{i}.git", i % 40)))
        .collect();
    c.bench_function("decide 3 marks for 1000 repos, 40 patterns", |b| {
        b.iter(|| {
            for id in &ids {
                for m in Mark::ALL {
                    black_box(rs.decide(m, Some(id), Path::new("/x")));
                }
            }
        })
    });
}

/// A packed repo with `n` commits on `main`, a tag only on an unrelated
/// branch, and HEAD detached at `main`'s tip: describe's worst case, where
/// no tag is reachable and the walk covers all of history.
fn untagged_history(sb: &Sandbox, n: usize, commit_graph: bool) -> PathBuf {
    use std::io::Write;
    let r = sb.path(&format!("hist-{n}-{commit_graph}"));
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    let mut script = String::new();
    for i in 1..=n {
        let msg = format!("c{i}");
        script.push_str(&format!(
            "commit refs/heads/main\ncommitter a <a@b> {} +0000\ndata {}\n{msg}\n",
            1_700_000_000 + i,
            msg.len()
        ));
        if i == 1 {
            script.push_str("deleteall\n");
        }
    }
    script.push_str(
        "commit refs/heads/other\ncommitter a <a@b> 1600000000 +0000\ndata 5\nother\ndeleteall\n",
    );
    let mut p = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&r)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    p.stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(p.wait().unwrap().success());
    git(&r, &["tag", "v1", "other"]);
    git(&r, &["checkout", "-q", "--detach", "main"]);
    git(&r, &["gc", "-q"]);
    git(&r, &["commit-graph", "write", "--reachable"]);
    if !commit_graph {
        std::fs::remove_file(r.join(".git/objects/info/commit-graph")).ok();
        std::fs::remove_dir_all(r.join(".git/objects/info/commit-graphs")).ok();
    }
    r
}

fn describe(c: &mut Criterion) {
    let sb = Sandbox::new();
    let mut g = c.benchmark_group("describe, no reachable tag");
    g.sample_size(20);
    for n in [1_000, 10_000, 50_000] {
        for graph in [false, true] {
            let r = untagged_history(&sb, n, graph);
            let repo = gix::open(&r).unwrap();
            let name = format!("{n} commits{}", if graph { ", commit-graph" } else { "" });
            g.bench_function(name, |b| {
                b.iter(|| {
                    let head = repo.head_commit().unwrap();
                    black_box(
                        head.describe()
                            .names(gix::commit::describe::SelectRef::AllTags)
                            .try_resolve()
                            .unwrap()
                            .is_some(),
                    )
                })
            });
        }
    }
    g.finish();
}

criterion_group!(
    benches,
    discovery,
    status_and_scan,
    diff,
    config,
    rules,
    describe
);
criterion_main!(benches);
