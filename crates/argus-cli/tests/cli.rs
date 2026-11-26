//! End-to-end CLI tests: the real binary against sandbox repositories.

use argus_core::icons::{ASCII, NERD, UNICODE};
use argus_core::testkit::{Sandbox, commit, git, git_may_fail, subprocess_env, write};
use assert_cmd::Command;
use predicates::prelude::*;

fn argus(sb: &Sandbox) -> Command {
    let mut c = Command::cargo_bin("argus").unwrap();
    for (k, v) in subprocess_env(&sb.root()) {
        c.env(k, v);
    }
    c.env_remove("ARGUS_CONFIG").env_remove("ARGUS_STATE");
    c.arg("--root").arg(sb.path("dev"));
    c
}

fn three(sb: &Sandbox) {
    for n in ["clean", "dirty", "feature"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty"), "README", "changed\n");
    let f = sb.path("dev/feature");
    git(&f, &["switch", "-q", "-c", "feat"]);
    commit(&f, "feat");
    git(&f, &["push", "-q", "-u", "origin", "feat"]);
}

#[test]
fn status_text_and_json() {
    let sb = Sandbox::new();
    three(&sb);
    // default: clean repos are left out, and the footer says how to see them
    argus(&sb)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/dirty").and(predicate::str::contains("±1")))
        .stdout(predicate::str::contains("acme/clean").not())
        .stdout(predicate::str::contains(
            "2 of 3 repos need attention; 1 not shown needs no attention (--all)",
        ))
        .stdout(predicate::str::contains("\x1b[").not());
    // inverse: --all / -a includes them
    for all in ["--all", "-a"] {
        argus(&sb)
            .args(["status", all])
            .assert()
            .success()
            .stdout(predicate::str::contains("acme/clean"))
            .stdout(predicate::str::contains("3 repos, 2 need attention"))
            .stdout(predicate::str::contains("not shown").not());
    }

    let names = |args: &[&str]| -> Vec<String> {
        let out = argus(&sb).args(args).output().unwrap();
        assert!(out.status.success());
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"]["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&["status", "--json"]), vec!["dirty", "feature"]);
    assert_eq!(
        names(&["status", "--json", "--all"]),
        vec!["dirty", "feature", "clean"]
    );
}

#[test]
fn status_when_everything_is_clean() {
    let sb = Sandbox::new();
    sb.remote("acme", "clean");
    sb.clone_as_github("acme", "clean", "dev/clean");
    argus(&sb)
        .arg("status")
        .assert()
        .success()
        .stdout("0 of 1 repo need attention; 1 not shown needs no attention (--all)\n");
}

/// argus with an isolated environment but no `--root`, run from `cwd`.
fn argus_in(sb: &Sandbox, cwd: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("argus").unwrap();
    for (k, v) in subprocess_env(&sb.root()) {
        c.env(k, v);
    }
    c.env_remove("ARGUS_CONFIG")
        .env_remove("ARGUS_STATE")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_DISCOVERY_ACROSS_FILESYSTEM")
        .current_dir(cwd);
    c
}

#[test]
fn no_roots_scans_current_directory_with_a_hint() {
    let sb = Sandbox::new();
    three(&sb);
    argus_in(&sb, &sb.path("dev"))
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("of 3 repos"))
        .stderr(predicate::str::contains(
            "no roots configured; scanning the current directory",
        ));
    // inverse: configured roots win over the current directory, and no hint
    argus_in(&sb, &sb.root())
        .args(["root", "add", "dev"])
        .assert()
        .success();
    argus_in(&sb, &sb.path("dev/clean"))
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("of 3 repos"))
        .stderr(predicate::str::contains("no roots configured").not());
}

const SCANNING_HERE: &str =
    "no roots configured; scanning the current directory (save it with `argus root add`)";

/// From inside a repo, its outermost superproject is scanned as well as
/// the current directory, so the repo you're in is listed.
#[test]
fn no_roots_inside_a_repo_scans_the_repo_it_is_in_too() {
    let sb = Sandbox::new();
    let top = sb.nested_submodules();
    let in_top = "no roots configured; scanning the current directory and ~/top, \
                  the repo it's in (save them with `argus root add`)";
    for from in ["mid/leaf/deep", "mid"] {
        argus_in(&sb, &top.join(from))
            .arg("status")
            .assert()
            .success()
            .stdout(predicate::str::contains("of 3 repos"))
            .stderr(predicate::str::contains(in_top));
    }
    // inverse: at the top, the current directory is the repo
    argus_in(&sb, &top)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("of 3 repos"))
        .stderr(predicate::str::contains(SCANNING_HERE));
    // and outside every repo, only the current directory
    argus_in(&sb, &sb.path("src"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains(SCANNING_HERE));
}

/// The repo is found as git finds it: not past `GIT_CEILING_DIRECTORIES`.
#[test]
fn no_roots_looks_for_the_repo_it_is_in_within_gits_ceilings() {
    let sb = Sandbox::new();
    let top = sb.init("top");
    std::fs::create_dir_all(top.join("a/b")).unwrap();
    std::fs::create_dir_all(sb.path("elsewhere")).unwrap();
    let run = |ceiling: &std::path::Path| {
        argus_in(&sb, &top.join("a/b"))
            .env("GIT_CEILING_DIRECTORIES", ceiling)
            .arg("status")
            .assert()
            .success()
    };
    run(&top.join("a")).stderr(predicate::str::contains(SCANNING_HERE));
    // inverse: a ceiling elsewhere doesn't stop it (nor is it an error)
    run(&sb.path("elsewhere")).stderr(predicate::str::contains("~/top, the repo it's in"));
}

/// Nor onto another filesystem, unless `GIT_DISCOVERY_ACROSS_FILESYSTEM`
/// says so.
#[cfg(target_os = "macos")]
#[test]
fn no_roots_looks_for_the_repo_it_is_in_on_its_filesystem_only() {
    let sb = Sandbox::new();
    let top = sb.init("top");
    let mounted = argus_core::testkit::Mounted::new(&sb, "top/mnt");
    std::fs::create_dir_all(mounted.0.join("here")).unwrap();
    argus_in(&sb, &mounted.0.join("here"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains(SCANNING_HERE));
    // inverse: allowed across
    argus_in(&sb, &mounted.0.join("here"))
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "true")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains("~/top, the repo it's in"));
    drop(top);
}

#[test]
fn first_run_writes_commented_example_without_roots() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    let cfg = sb.path(".config/argus/config.toml");
    argus_in(&sb, &sb.path("dev"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "created ~/.config/argus/config.toml",
        ));
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        text.contains("# roots = []") && !text.lines().any(|l| l.starts_with("roots")),
        "roots never guessed"
    );
    assert_eq!(text, argus_core::configfile::example());
    // second run: no rewrite, no message
    std::fs::write(&cfg, "# mine\n").unwrap();
    argus_in(&sb, &sb.path("dev"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains("created").not());
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "# mine\n");
}

#[test]
fn root_add_list_remove() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    std::fs::create_dir_all(sb.path("work")).unwrap();
    let a = |args: &[&str]| argus_in(&sb, &sb.path("dev")).args(args).assert();
    a(&["root", "list"])
        .success()
        .stdout(predicate::str::contains("no roots configured"));
    a(&["root", "add"])
        .success()
        .stdout(predicate::str::contains("added ~/dev"));
    a(&["root", "add", "../work", "."])
        .success()
        .stdout("~/dev is already configured\nadded ~/work\n");
    a(&["root", "add"])
        .success()
        .stdout(predicate::str::contains("already configured"));
    a(&["root", "list"]).success().stdout("~/dev\n~/work\n");
    a(&["root", "add", "nope"])
        .failure()
        .stderr(predicate::str::contains("no such directory"));
    std::fs::remove_dir(sb.path("work")).unwrap();
    a(&["root", "list"])
        .success()
        .stdout(predicate::str::contains("~/work  (missing)"));
    a(&["root", "remove", "~/work"])
        .success()
        .stdout("removed ~/work\n");
    a(&["root", "remove", "~/work"])
        .success()
        .stdout("")
        .stderr(predicate::str::contains(
            "~/work is not configured as a root",
        ));
    a(&["root", "list"]).success().stdout("~/dev\n");
    // the generated documentation survived the edits
    let text = std::fs::read_to_string(sb.path(".config/argus/config.toml")).unwrap();
    assert!(
        text.contains("## Maximum directory depth below a root"),
        "{text}"
    );
}

/// A folder walked for nothing, a thousand folders deep: named, to skip.
#[test]
fn warns_of_a_folder_walked_for_nothing() {
    let sb = Sandbox::new();
    sb.init("dev/app");
    for i in 0..1000 {
        std::fs::create_dir_all(sb.path(&format!("dev/data/{}/{i}", i % 10))).unwrap();
    }
    argus(&sb)
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "walked 1011 folders in ~/dev/data and found no repo: exclude_paths skips it",
        ));
    // inverse: skipped, nothing said
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "extend_exclude_paths = ['data']\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::contains("walked").not());
}

#[test]
fn warns_when_max_depth_stops_the_scan() {
    let sb = Sandbox::new();
    sb.init("dev/a/b/c/deep");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "max_depth = 2\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("0 repos"))
        .stderr(predicate::str::contains(
            "1 directory below max_depth (2) not scanned, e.g. ~/dev/a/b; raise max_depth",
        ));
    std::fs::write(&cfg, "max_depth = 6\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 repo, 1 needs attention"))
        .stderr(predicate::str::contains("max_depth").not());
    // --max-depth overrides the config, both ways
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["--max-depth", "2", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 repos"))
        .stderr(predicate::str::contains("below max_depth (2)"));
    std::fs::write(&cfg, "max_depth = 2\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["status", "--max-depth", "4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 repo, 1 needs attention"))
        .stderr(predicate::str::contains("max_depth").not());
    // 0: no limit
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["status", "--max-depth", "0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 repo, 1 needs attention"))
        .stderr(predicate::str::contains("max_depth").not());
}

#[test]
fn fix_dry_run_then_apply() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .args(["fix", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/feature: would switch main"));
    assert!(
        git(&sb.path("dev/feature"), &["branch", "--show-current"]).trim() == "feat",
        "dry run changes nothing"
    );
    argus(&sb)
        .arg("fix")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ switch main"));
    assert_eq!(
        git(&sb.path("dev/feature"), &["branch", "--show-current"]).trim(),
        "main"
    );
    argus(&sb)
        .arg("fix")
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to fix"));
}

/// `-v` prints each step argus takes, with how long it took, on stderr:
/// here the scan, each repo's status and the git a fix runs.
#[test]
fn verbose_prints_each_step_and_its_time_on_stderr() {
    let sb = Sandbox::new();
    three(&sb);
    let timed = |what: &str| {
        predicate::str::is_match(format!(r"argus: +(<1ms|\d+ms|\d+\.\d\ds)  {what}")).unwrap()
    };
    argus(&sb)
        .args(["-v", "fix", "-k", "stash", "acme/dirty"])
        .assert()
        .success()
        .stderr(timed("config "))
        .stderr(timed("discover in ~/dev: 3 repos"))
        .stderr(timed("statuses of 3 repos"))
        .stderr(timed(r"status ~/dev/dirty( \(.*\))?\n"))
        .stderr(timed("git stash "))
        .stdout(predicate::str::contains("status ~/dev").not());
    // inverse: without it, none
    argus(&sb)
        .args(["fix", "-k", "stash", "acme/feature"])
        .assert()
        .success()
        .stderr(predicate::str::contains("discover in").not())
        .stderr(predicate::str::contains("status ~/dev").not());
}

/// A fake `git` first on `PATH`: asked for its installation config (as
/// gix asks), it names the file `$FAKE_GIT_ANSWER` holds (none if it's
/// empty), after `$FAKE_GIT_DELAY` seconds; for all else it runs git.
fn fake_git(sb: &Sandbox) -> std::path::PathBuf {
    let out = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let bin = sb.path("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = config ] && [ \"$2\" = -lz ]; then\n\
         \tsleep \"${{FAKE_GIT_DELAY:-0}}\"\n\
         \tanswer=$(cat \"$FAKE_GIT_ANSWER\")\n\
         \t[ -n \"$answer\" ] && printf '%s\\0file:%s\\0core.excludesfile\\0' \"${{FAKE_GIT_SCOPE:-system}}\" \"$answer\"\n\
         \texit 0\n\
         fi\n\
         exec {real} \"$@\"\n"
    );
    let git = bin.join("git");
    std::fs::write(&git, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// argus with the fake git, reading git's installation config.
fn argus_with_fake_git(sb: &Sandbox, bin: &std::path::Path) -> Command {
    let mut c = argus(sb);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    c.env("PATH", path)
        .env("FAKE_GIT_ANSWER", sb.path("answer"))
        .env_remove("GIT_CONFIG_NOSYSTEM");
    c
}

/// How many untracked files `argus --json status --all` says `r` has.
fn untracked_in(out: &[u8], r: &std::path::Path) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    let entry = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"].as_str() == Some(r.to_str().unwrap()))
        .unwrap_or_else(|| panic!("{r:?} in {v}"));
    entry["status"]["Ok"]["changes"]["untracked"]
        .as_u64()
        .unwrap_or_else(|| panic!("{entry}"))
}

/// Git's installation config is remembered, and repos opened with it at
/// once; git, asked alongside, has the last word: when it names another,
/// the statuses are read again with that one, before anything's shown.
#[test]
fn gits_installation_config_is_remembered_and_checked_alongside() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    write(&r, "junk", "x\n");
    let bin = fake_git(&sb);
    write(&sb.root(), "ignores", "junk\n");
    let ignoring = sb.path("ignoring.gitconfig");
    let excludes = format!(
        "[core]\n\texcludesFile = {}\n",
        sb.path("ignores").display()
    );
    std::fs::write(&ignoring, excludes).unwrap();
    let plain = sb.path("plain.gitconfig");
    std::fs::write(&plain, "").unwrap();
    let answer = |a: &str| std::fs::write(sb.path("answer"), a).unwrap();
    let kept = sb.path(".state/argus/git-installation");
    let run = || {
        let o = argus_with_fake_git(&sb, &bin)
            .args(["-v", "--json", "status", "--all"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{o:?}");
        (
            untracked_in(&o.stdout, &r),
            String::from_utf8(o.stderr).unwrap(),
        )
    };
    const AGAIN: &str = "statuses read again";
    // first: git asked as repos open, and its answer kept
    answer(ignoring.to_str().unwrap());
    let (n, err) = run();
    assert_eq!(n, 0, "junk ignored by the installation config\n{err}");
    assert!(!err.contains(AGAIN), "{err}");
    let line = format!("system\t{}\n", ignoring.display());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), line);
    // git now names another: opened with the kept one, read again
    answer(plain.to_str().unwrap());
    let (n, err) = run();
    assert_eq!(n, 1, "{err}");
    assert!(err.contains(AGAIN), "{err}");
    let line = format!("system\t{}\n", plain.display());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), line);
    // and with that kept, nothing to read again
    let (n, err) = run();
    assert_eq!(n, 1);
    assert!(!err.contains(AGAIN), "{err}");
    // none at all is an answer too
    answer("");
    let (n, err) = run();
    assert_eq!(n, 1);
    assert!(err.contains(AGAIN), "{err}");
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "none\n");
    answer(ignoring.to_str().unwrap());
    let (n, err) = run();
    assert_eq!(n, 0, "{err}");
    assert!(err.contains(AGAIN), "{err}");
}

/// As git: `GIT_CONFIG_SYSTEM` replaces an installation config in the
/// system scope (Homebrew's), not one in another (Apple's).
#[test]
fn git_config_system_replaces_a_kept_system_scoped_installation_config() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    write(&r, "junk", "x\n");
    let bin = fake_git(&sb);
    write(&sb.root(), "ignores", "junk\n");
    let ignoring = sb.path("ignoring.gitconfig");
    let excludes = format!(
        "[core]\n\texcludesFile = {}\n",
        sb.path("ignores").display()
    );
    std::fs::write(&ignoring, excludes).unwrap();
    let plain = sb.path("plain.gitconfig");
    std::fs::write(&plain, "").unwrap();
    std::fs::write(sb.path("answer"), ignoring.to_str().unwrap()).unwrap();
    let untracked = |scope: &str, system: Option<&std::path::Path>| {
        let mut c = argus_with_fake_git(&sb, &bin);
        c.env("FAKE_GIT_SCOPE", scope)
            .args(["--json", "status", "--all"]);
        if let Some(s) = system {
            c.env("GIT_CONFIG_SYSTEM", s);
        }
        untracked_in(&c.output().unwrap().stdout, &r)
    };
    // (each first run keeps the answer; the second opens with it kept)
    for _ in 0..2 {
        assert_eq!(untracked("system", None), 0);
        assert_eq!(untracked("system", Some(&plain)), 1, "replaced");
    }
    for _ in 0..2 {
        assert_eq!(untracked("unknown", Some(&plain)), 0, "not replaced");
    }
}

/// With the answer kept, opening a repo doesn't wait for git.
#[test]
fn a_kept_installation_config_spares_repos_waiting_for_git() {
    let sb = Sandbox::new();
    sb.init("dev/r");
    let bin = fake_git(&sb);
    std::fs::write(sb.path("answer"), "").unwrap();
    let opened = |delay: &str| {
        let o = argus_with_fake_git(&sb, &bin)
            .env("FAKE_GIT_DELAY", delay)
            .args(["-v", "status"])
            .output()
            .unwrap();
        let err = String::from_utf8(o.stderr).unwrap();

        err.lines()
            .find(|l| l.ends_with("  status ~/dev/r: open"))
            .unwrap_or_else(|| panic!("{err}"))
            .to_string()
    };
    // how long the open took, in ms
    let open_ms = |line: &str| -> f64 {
        let took = line
            .trim_start_matches("argus:")
            .split_whitespace()
            .next()
            .unwrap_or_default();
        match took.strip_suffix("ms") {
            Some(ms) => ms.parse().unwrap_or(0.0),
            None => took.trim_end_matches('s').parse::<f64>().unwrap_or(0.0) * 1000.0,
        }
    };
    // first, nothing kept: the open waits for git (its second, less what
    // passed before the open: git is asked from the start)
    let first = opened("1");
    assert!(open_ms(&first) >= 500.0, "{first}");
    // then kept: it doesn't
    let kept = opened("1");
    assert!(open_ms(&kept) < 500.0, "{kept}");
}

#[test]
fn fix_with_explicit_kind_and_pattern() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .args(["fix", "-k", "stash", "acme/clean"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to fix"));
    // the result mark is an icon: the ascii set's here
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[ui]\nicons = 'ascii'\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["fix", "-k", "stash", "acme/dirty"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/dirty: ok stash"))
        .stdout(predicate::str::contains("✓").not());
    assert_eq!(
        git(&sb.path("dev/dirty"), &["stash", "list"])
            .lines()
            .count(),
        1
    );
    write(&sb.path("dev/dirty"), "README", "changed again\n");
    // -k takes a comma-separated list
    argus(&sb)
        .args(["fix", "-n", "-k", "push,switch-default", "acme/feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/feature: would switch main"));
    // repeated -k flags combine, each with its own comma list
    argus(&sb)
        .args(["fix", "-n", "-k", "publish,stash", "--kind", "pull"])
        .args(["-k", "switch-default", "acme/dirty", "acme/feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/dirty: would stash"))
        .stdout(predicate::str::contains("acme/feature: would switch main"));
    // inverse: without the second flag, feature only gets a hint
    argus(&sb)
        .args([
            "fix",
            "-n",
            "-k",
            "publish,stash",
            "acme/dirty",
            "acme/feature",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/dirty: would stash"))
        .stdout(predicate::str::contains("acme/feature: nothing automatic"));
    // the newer kinds by name: a detached HEAD at main is reattached
    git(&sb.path("dev/clean"), &["switch", "-q", "--detach", "main"]);
    argus(&sb)
        .args(["fix", "-n", "-k", "push-branches,reattach", "acme/clean"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "acme/clean: would switch main (HEAD is at main)",
        ));
    argus(&sb)
        .args(["fix", "-k", "reattach", "acme/clean"])
        .assert()
        .success();
    assert_eq!(
        git(&sb.path("dev/clean"), &["branch", "--show-current"]).trim(),
        "main"
    );
    // a kind that's never automatic, in [fix] auto: warned about
    let cfg = sb.path(".config/argus/config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[fix]\nauto = ['push', 'commit']\n").unwrap();
    argus(&sb)
        .args(["fix", "-n"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "argus: [fix] auto lists commit, which need input or confirmation",
        ));
    std::fs::remove_file(&cfg).unwrap();
    argus(&sb)
        .args(["fix", "-n"])
        .assert()
        .success()
        .stderr(predicate::str::contains("[fix] auto").not());
    // an unknown kind is rejected
    argus(&sb)
        .args(["fix", "-n", "-k", "push,shove"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value 'shove'"));
}

#[test]
fn tag_snooze_and_ack_roundtrip_through_state() {
    let sb = Sandbox::new();
    three(&sb);
    // favourites lead their group, but unclean repos still come first
    argus(&sb)
        .args(["tag", "favourite", "acme/clean", "acme/feature"])
        .assert()
        .success();
    let out = argus(&sb).args(["status", "--all"]).output().unwrap();
    let lines: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let order: Vec<&str> = lines
        .iter()
        .filter_map(|l| {
            ["feature", "dirty", "clean"]
                .into_iter()
                .find(|n| l.contains(&format!("acme/{n}")))
        })
        .collect();
    assert_eq!(order, ["feature", "dirty", "clean"], "{lines:?}");
    assert!(
        lines[0].contains("◆") && lines[2].contains("◆"),
        "{lines:?}"
    );

    argus(&sb)
        .args(["snooze", "acme/dirty", "--for", "2d"])
        .assert()
        .success();
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty").not())
        .stdout(predicate::str::contains("1 hidden (--hidden)"));
    argus(&sb)
        .args(["status", "--all"])
        .assert()
        .stdout(predicate::str::contains("acme/dirty").not());
    argus(&sb)
        .args(["status", "--hidden"])
        .assert()
        .stdout(predicate::str::contains("acme/dirty"))
        .stdout(predicate::str::contains("hidden (--hidden)").not());
    argus(&sb)
        .args(["unsnooze", "acme/dirty"])
        .assert()
        .success();
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty"));

    argus(&sb)
        .args(["snooze", "acme/*", "--for", "soon"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad duration"));
    argus(&sb)
        .args(["tag", "ignore", "nomatch/*"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no repos match"));
    argus(&sb)
        .arg("ack")
        .assert()
        .success()
        .stdout(predicate::str::contains("acknowledged 0"));
}

#[test]
fn pull_pulls_tagged_repos_and_reports_failures_with_exit_code() {
    let sb = Sandbox::new();
    three(&sb);
    sb.advance_remote("acme", "clean", "upstream-file");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "tags.pull.repos = ['acme/clean']\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would pull acme/clean"));
    assert!(!sb.path("dev/clean/upstream-file").exists());
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("pull")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 jobs: 1 ok"));
    assert!(sb.path("dev/clean/upstream-file").exists());

    // break the remote (keeping its identity): non-zero exit, and the error is shown
    let c = sb.path("dev/clean");
    git(
        &c,
        &[
            "config",
            "--remove-section",
            &format!("url.{}/", sb.path("forge").display()),
        ],
    );
    git(
        &c,
        &[
            "config",
            "url./nonexistent/.insteadOf",
            "https://github.com/",
        ],
    );
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("pull")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("failed: pull acme/clean"));
}

#[test]
fn bad_config_is_reported() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "rootz = []\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("rootz"));
}

#[test]
fn each_fix_run_applies_one_fix() {
    let sb = Sandbox::new();
    three(&sb);
    let f = sb.path("dev/feature");
    commit(&f, "unpushed");
    argus(&sb)
        .args(["fix", "acme/feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ push").and(predicate::str::contains("switch").not()));
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "feat");
    argus(&sb)
        .args(["fix", "acme/feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ switch main"));
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "main");
}

#[test]
fn help_documents_every_flag_and_repeatable_options() {
    let sb = Sandbox::new();
    for cmd in [
        &["status"][..],
        &["pull"],
        &["fix"],
        &["tag"],
        &["untag"],
        &["snooze"],
        &["unsnooze"],
        &["ignore-issue"],
        &["unignore-issue"],
        &["ack"],
        &["forge", "list"],
        &["root", "add"],
        &["root", "remove"],
        &["demo"],
    ] {
        let out = argus(&sb).args(cmd).arg("--help").output().unwrap();
        let help = String::from_utf8(out.stdout).unwrap();
        for line in help.lines().filter(|l| l.trim_start().starts_with('-')) {
            let l = line.trim_start();
            assert!(
                l.starts_with("--") || l.contains(", --"),
                "no long form: {l}"
            );
            assert!(
                l.split("  ").filter(|s| !s.is_empty()).count() >= 2,
                "{cmd:?} undocumented: {l}"
            );
        }
    }
    let fix = argus(&sb).args(["fix", "--help"]).output().unwrap();
    assert!(
        String::from_utf8(fix.stdout)
            .unwrap()
            .contains("comma-separated, repeatable")
    );
    let top = argus(&sb).arg("--help").output().unwrap();
    assert!(
        String::from_utf8(top.stdout)
            .unwrap()
            .contains("(repeatable)")
    );
}

#[test]
fn dry_run_shows_the_fix_the_chain_and_alternatives() {
    let sb = Sandbox::new();
    three(&sb);
    write(&sb.path("dev/feature"), "notes.txt", "untracked\n");
    let out = |args: &[&str]| {
        let o = argus(&sb).args(args).output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stdout).unwrap()
    };
    // default [fix] auto = push, push-branches, switch-default, reattach:
    // stash is only an alternative
    let s = out(&["fix", "-n", "acme/feature", "acme/dirty"]);
    assert_eq!(
        s,
        "acme/dirty: nothing automatic\n  \
         alternative: stash (1 uncommitted path(s)), with -k stash\n\
         acme/feature: would switch main (feat has nothing unpushed)\n  \
         alternative: stash (1 uncommitted path(s)), with -k stash\n"
    );
    // both allowed: what later runs would do is the chain, not an alternative
    assert_eq!(
        out(&["fix", "-n", "-k", "stash,switch-default", "acme/feature"]),
        "acme/feature: would stash (1 uncommitted path(s))\n  \
         then: switch main (feat has nothing unpushed)\n"
    );
    // reversed priority: switch first, and the untracked file still needs a stash
    assert_eq!(
        out(&["fix", "-n", "-k", "switch-default,stash", "acme/feature"]),
        "acme/feature: would switch main (feat has nothing unpushed)\n  \
         then: stash (1 uncommitted path(s))\n"
    );
}

#[test]
fn dry_run_explains_why_a_pull_is_not_automatic() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "dev/w", true);
    git(&r, &["config", "pull.rebase", "false"]);
    let o = argus(&sb)
        .args(["fix", "-n", "-k", "pull"])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "acme/w: nothing automatic\n  \
         alternative: pull (diverged from origin/feat: 1 ahead, 1 behind); \
         not automatic: would conflict in README\n"
    );
    // inverse: a clean merge is applied, then push
    git(&sb.path("dev/w"), &["reset", "-q", "--hard", "HEAD~1"]);
    write(&r, "ours.txt", "ours\n");
    commit(&r, "ours, no conflict");
    let o = argus(&sb)
        .args(["fix", "-n", "-k", "pull,push"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "acme/w: would pull (diverged from origin/feat: 1 ahead, 1 behind; merges cleanly, \
         adding a merge commit)\n  \
         then: push (2 commit(s) ahead of origin/feat)\n"
    );
}

#[test]
fn fix_fetch_updates_remotes_before_planning() {
    let sb = Sandbox::new();
    for n in ["r", "other"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
        sb.advance_remote("acme", n, "new.txt");
    }
    // without --fetch the remote commits are unknown
    let o = argus(&sb)
        .args(["fix", "-n", "-k", "pull"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(o.stdout).unwrap(), "nothing to fix\n");
    // --fetch only fetches the selected repos
    let o = argus(&sb)
        .args(["fix", "-n", "--fetch", "-k", "pull", "acme/r"])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "acme/r: would pull (1 commit(s) behind origin/main)\n"
    );
    let behind = |n: &str| {
        git(
            &sb.path(&format!("dev/{n}")),
            &["rev-list", "--count", "HEAD..origin/main"],
        )
        .trim()
        .to_string()
    };
    assert_eq!(behind("r"), "1");
    assert_eq!(behind("other"), "0");
    // and a real run applies the fix with fresh information
    argus(&sb)
        .args(["fix", "-F", "-k", "pull"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/other: ✓ pull"));
    assert_eq!(behind("other"), "0");
    assert!(sb.path("dev/other/new.txt").exists());
}

#[test]
fn fix_fetch_failures_are_reported_and_planning_continues() {
    let sb = Sandbox::new();
    sb.remote("acme", "gone");
    sb.clone_as_github("acme", "gone", "dev/gone");
    write(&sb.path("dev/gone"), "README", "changed\n");
    std::fs::remove_dir_all(sb.path("forge/acme/gone.git")).unwrap();
    let o = argus(&sb)
        .args(["fix", "-n", "--fetch", "-k", "stash"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let err = String::from_utf8(o.stderr).unwrap();
    assert!(err.contains("fetch failed: acme/gone"), "{err}");
    assert!(
        String::from_utf8(o.stdout)
            .unwrap()
            .starts_with("acme/gone: would stash"),
    );
}

#[cfg(unix)]
#[test]
fn follow_symlinks_flag_and_config() {
    let sb = Sandbox::new();
    sb.init("elsewhere/linked");
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    std::os::unix::fs::symlink(sb.path("elsewhere"), sb.path("dev/link")).unwrap();
    let status = |args: &[&str]| {
        let o = argus(&sb).args(args).arg("status").output().unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    assert!(status(&[]).contains("0 repos"));
    for flag in ["-L", "--follow-symlinks"] {
        assert!(
            status(&[flag]).contains("1 repo, 1 needs attention"),
            "{flag}"
        );
    }
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "follow_symlinks = true\n").unwrap();
    assert!(status(&["--config", cfg.to_str().unwrap()]).contains("1 repo, 1 needs attention"));
}

#[cfg(target_os = "macos")]
#[test]
fn one_file_system_stays_off_other_mounts() {
    let sb = Sandbox::new();
    sb.init("dev/here");
    sb.init("dev/deeper/also-here");
    let mounted = argus_core::testkit::Mounted::new(&sb, "dev/mnt");
    sb.init("dev/mnt/elsewhere");
    sb.init("outside/linked");
    let symlink =
        |to: &std::path::Path, at: &str| std::os::unix::fs::symlink(to, sb.path(at)).unwrap();
    symlink(&mounted.0.join("elsewhere"), "dev/onto-mount");
    symlink(&sb.path("outside"), "dev/same-fs");
    let names = |args: &[&str]| {
        let mut args = args.to_vec();
        args.extend(["status", "--all"]);
        let mut names: Vec<String> = json(&sb, &args)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                r["path"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        names.sort();
        names
    };
    // inverse: by default the walk crosses into the mount
    assert_eq!(names(&[]), ["also-here", "elsewhere", "here"]);
    for flag in ["-x", "--one-file-system"] {
        assert_eq!(names(&[flag]), ["also-here", "here"], "{flag}");
    }
    // a link onto another filesystem isn't followed; one on the same is
    assert_eq!(names(&["-L"]), ["also-here", "elsewhere", "here", "linked"]);
    assert_eq!(names(&["-L", "-x"]), ["also-here", "here", "linked"]);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "one_file_system = true\n").unwrap();
    assert_eq!(
        names(&["--config", cfg.to_str().unwrap()]),
        ["also-here", "here"]
    );
    // a root on the other filesystem is scanned: the boundary is each root's own
    let mount = mounted.0.to_str().unwrap().to_string();
    assert_eq!(
        names(&["-x", "--root", &mount]),
        ["also-here", "elsewhere", "here"],
        "with two roots, each keeps to its own"
    );
    let o = argus_in(&sb, &sb.root())
        .arg("--root")
        .arg(&mounted.0)
        .args(["-x", "status"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("1 repo,"), "{o:?}");
}

#[test]
fn tui_without_a_terminal_explains_instead_of_panicking() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    for args in [&[][..], &["tui"], &["demo"]] {
        argus(&sb)
            .args(args)
            .assert()
            .code(1)
            .stderr(predicate::str::contains("needs a terminal"))
            .stderr(predicate::str::contains("panicked").not());
    }
}

#[test]
fn icon_overrides_show_in_status_and_unknown_names_warn() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        "[ui.icon_overrides]\nunstaged = 'M'\nfavourit = '★'\n",
    )
    .unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("M1"))
        .stdout(predicate::str::contains("±1").not())
        .stderr(predicate::str::contains(
            "unknown icon in [ui.icon_overrides]: favourit",
        ));
}

#[test]
fn demo_glyphs_prints_every_icon_between_markers_without_a_terminal() {
    let sb = Sandbox::new();
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[ui.icon_overrides]\nstash = 'S'\n").unwrap();
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["demo", "--glyphs"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.contains("building"), "no repos built: {stderr}");
    let line = |name: &str| {
        stdout
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("no line for {name}:\n{stdout}"))
            .to_string()
    };
    // every icon, from every set, between bars and letters
    for name in argus_core::icons::ICON_NAMES {
        let l = line(name);
        for set in [&UNICODE, &NERD, &ASCII] {
            let g = set.get(name).unwrap();
            assert!(l.contains(&format!("|{g}|")), "{name}: {g}: {l}");
            assert!(l.contains(&format!("M{g}M")), "{name}: {g}: {l}");
        }
    }
    // the config's own icons, overrides included
    assert!(line("stash").contains("|S|"), "{}", line("stash"));
    // inverse: icons without an override keep the set's glyph
    assert!(line("ahead").ends_with(&format!("|{}|M{}M*", UNICODE.ahead, UNICODE.ahead)));
    // candidates, flagged when two cells wide in CJK mode
    assert!(line("◊").contains("U+25CA"));
    assert!(!line("◊").ends_with('*'), "◊ isn't ambiguous width");
    assert!(line("★").ends_with('*'), "★ is ambiguous width");
    // punctuation used as text
    for c in argus_core::icons::PUNCTUATION.chars() {
        assert!(
            stdout
                .lines()
                .any(|l| l.contains(&format!("|{c}|M{c}M")) && l.contains("text")),
            "{c}:\n{stdout}"
        );
    }
}

#[test]
fn completions_for_each_shell() {
    let sb = Sandbox::new();
    // the scripts register argus to be asked for candidates while typing
    for (shell, marker) in [
        ("bash", "complete -o"),
        ("zsh", "#compdef argus"),
        ("fish", "complete --keep-order"),
        ("elvish", "edit:completion:arg-completer[argus]"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let out = argus(&sb).args(["completions", shell]).output().unwrap();
        assert!(out.status.success(), "{shell}: {out:?}");
        let script = String::from_utf8(out.stdout).unwrap();
        assert!(script.contains(marker), "{shell}:\n{script}");
        assert!(
            script.contains(&format!("COMPLETE={shell}")) || script.contains("COMPLETE"),
            "{shell}:\n{script}"
        );
        // where the shell is installed, the script must parse
        if matches!(shell, "bash" | "zsh" | "fish") {
            let file = sb.path(&format!("completion.{shell}"));
            std::fs::write(&file, &script).unwrap();
            let check = if shell == "fish" {
                "--no-execute"
            } else {
                "-n"
            };
            if let Ok(st) = std::process::Command::new(shell)
                .arg(check)
                .arg(&file)
                .status()
            {
                assert!(st.success(), "{shell} rejects its script");
            }
        }
    }
    // inverse: unknown shells are rejected
    argus(&sb).args(["completions", "tcsh"]).assert().failure();
    // printing a script creates no config file
    let cfg = sb.path("never.toml");
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["completions", "zsh"])
        .assert()
        .success();
    assert!(!cfg.exists());
}

/// Run with `--json`; stdout must be exactly one JSON document.
fn json(sb: &Sandbox, args: &[&str]) -> serde_json::Value {
    let out = argus(sb).arg("--json").args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: {e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn every_command_prints_json() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    let c = |args: &[&str]| -> Vec<String> {
        let mut v = vec!["--config".to_string(), cfg.display().to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        v
    };
    let run = |args: &[&str]| {
        let v = c(args);
        json(&sb, &v.iter().map(String::as_str).collect::<Vec<_>>())
    };
    // the flag works before or after the command
    let v = run(&["status"]);
    assert!(v.as_array().unwrap().iter().any(|e| e["key"].is_string()));
    let after = argus(&sb).args(c(&["status", "--json"])).output().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&after.stdout).unwrap(),
        v
    );

    let v = run(&["paths"]);
    assert_eq!(v["config"], cfg.display().to_string());
    assert!(v["state"].is_string());

    let dev = sb.path("dev").display().to_string();
    let v = run(&["root", "add", &dev]);
    assert_eq!(v["added"].as_array().unwrap().len(), 1, "{v}");
    let v = run(&["root", "list"]);
    assert_eq!(v[0]["exists"], true, "{v}");
    assert!(v[0]["path"].is_string());

    let v = run(&["tag", "pull", "acme/clean"]);
    assert_eq!(v[0]["repo"], "acme/clean", "{v}");
    assert_eq!(v[0]["pull"], true, "{v}");
    assert!(v[0]["key"].is_string());

    let v = run(&["pull", "--no-clone", "--dry-run"]);
    assert_eq!(v["dry_run"], true);
    let jobs = v["jobs"].as_array().unwrap();
    assert!(jobs.iter().all(|j| j["outcome"] == "planned"), "{v}");
    assert!(
        jobs.iter()
            .any(|j| j["label"].as_str().unwrap().contains("acme/clean")),
        "{v}"
    );
    let v = run(&["pull", "--no-clone"]);
    assert!(
        v["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|j| j["outcome"] == "ok"),
        "{v}"
    );
    assert_eq!(v["summary"]["failed"], 0, "{v}");

    let v = run(&["fix", "--dry-run"]);
    let feature = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["repo"] == "acme/feature")
        .unwrap();
    assert_eq!(feature["planned"]["kind"], "switch-default", "{v}");
    assert!(feature["planned"]["reason"].is_string());
    let v = run(&["fix", "acme/feature"]);
    assert_eq!(v[0]["applied"]["kind"], "switch-default", "{v}");
    assert_eq!(v[0]["ok"], true, "{v}");
    // nothing to do: an empty list, not a message
    assert_eq!(run(&["fix", "acme/feature"]), serde_json::json!([]));

    let v = run(&["ack"]);
    assert!(v["acknowledged"].is_array(), "{v}");

    let v = run(&["root", "remove", &dev]);
    assert_eq!(v["removed"].as_array().unwrap().len(), 1, "{v}");

    let v = run(&["demo", "--glyphs"]);
    assert_eq!(v["sets"]["unicode"]["tag.favourite"], "◆", "{v}");
    assert_eq!(v["sets"]["ascii"]["ok"], "ok", "{v}");
    assert!(v["punctuation"].is_string() && v["alternatives"].is_string());

    // inverse: commands without JSON output say so
    for args in [&["completions", "zsh"][..], &["tui"], &["demo"]] {
        argus(&sb)
            .arg("--json")
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("--json"));
    }
}

#[test]
fn clearing_tags_of_several_repos_needs_confirmation() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .args(["tag", "favourite", "acme/*"])
        .assert()
        .success();
    let favourites = |sb: &Sandbox| -> usize {
        let v = json(sb, &["status", "--all"]);
        v.as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["flags"]["tags"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m == "favourite")
            })
            .count()
    };
    assert_eq!(favourites(&sb), 3);
    // no terminal to ask on, and no --yes: refused, nothing changed
    argus(&sb)
        .args(["untag", "--all", "acme/*"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("clear tags of 3 repos"))
        .stderr(predicate::str::contains("--yes"));
    assert_eq!(favourites(&sb), 3);
    // one repo: no question
    argus(&sb)
        .args(["untag", "--all", "acme/clean"])
        .assert()
        .success();
    assert_eq!(favourites(&sb), 2);
    // --yes / -y proceed
    argus(&sb)
        .args(["untag", "--all", "acme/*", "--yes"])
        .assert()
        .success();
    assert_eq!(favourites(&sb), 0);
    argus(&sb)
        .args(["tag", "favourite", "acme/*"])
        .assert()
        .success();
    argus(&sb)
        .args(["-y", "untag", "--all", "acme/*"])
        .assert()
        .success();
    assert_eq!(favourites(&sb), 0);
    // repos with nothing to lose don't count
    argus(&sb)
        .args(["tag", "pull", "acme/dirty"])
        .assert()
        .success();
    argus(&sb)
        .args(["untag", "--all", "acme/*"])
        .assert()
        .success();
}

#[test]
fn tags_are_written_to_the_config_lists() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "# mine\ntags.pull.repos = ['acme/*']\n").unwrap();
    let run = |args: &[&str]| {
        argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .assert()
            .success();
    };
    run(&["tag", "favourite", "acme/clean"]);
    run(&["untag", "pull", "acme/dirty"]);
    run(&["tag", "ignore", "acme/feature"]);
    let text = std::fs::read_to_string(&cfg).unwrap();
    let c = argus_core::Config::parse(&text).unwrap();
    assert_eq!(c.tags["favourite"].repos, ["github.com/acme/clean"]);
    // ignoring also turns off pulling, which the pattern gave it
    assert_eq!(
        c.tags["pull"].repos,
        [
            "acme/*",
            "!github.com/acme/dirty",
            "!github.com/acme/feature"
        ]
    );
    assert_eq!(c.tags["ignore"].repos, ["github.com/acme/feature"]);
    assert!(text.starts_with("# mine\n"), "{text}");
    // --all: the repo's own entries go; what patterns still give is said
    let clear = |repo: &str| {
        let o = argus(&sb)
            .args(["--config", cfg.to_str().unwrap(), "untag", "--all", repo])
            .output()
            .unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stderr).unwrap()
    };
    assert_eq!(clear("acme/dirty"), "acme/dirty: still pull (acme/*)\n");
    let c = argus_core::Config::parse(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    // only acme/dirty's entries went; ignored acme/feature keeps its own
    assert_eq!(c.tags["pull"].repos, ["acme/*", "!github.com/acme/feature"]);
    // inverse: nothing left, nothing said
    std::fs::write(&cfg, "tags.favourite.repos = ['acme/clean']\n").unwrap();
    assert_eq!(clear("acme/clean"), "");
}

/// Tags were marks in 0.2.0: a config still saying `[marks]` is told what
/// they're called now.
#[test]
fn a_config_saying_marks_is_pointed_to_tags() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[marks.work]\nrepos = ['acme/dirty']\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "`marks` is now `[tags]`: move its entries there",
        ));
}

#[test]
fn old_marks_in_state_are_shown_as_config_and_kept() {
    let sb = Sandbox::new();
    three(&sb);
    let state = sb.path("state.toml");
    let old = "[marks.\"github.com/acme/clean\"]\nfavourite = true\n";
    std::fs::write(&state, old).unwrap();
    let run = |args: &[&str]| {
        argus(&sb)
            .arg("--state")
            .arg(&state)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["status"]);
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("tags.favourite.repos = [\"github.com/acme/clean\"]"),
        "{err}"
    );
    assert!(err.contains("config"), "{err}");
    // no longer used: clean isn't a favourite
    let v = json(
        &sb,
        &["--state", state.to_str().unwrap(), "status", "--all"],
    );
    let clean = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "github.com/acme/clean")
        .unwrap();
    assert_eq!(clean["flags"]["tags"], serde_json::json!([]));
    // a command that saves state keeps them
    assert!(run(&["ack"]).status.success());
    let text = std::fs::read_to_string(&state).unwrap();
    assert!(
        text.contains("[marks.\"github.com/acme/clean\"]") && text.contains("favourite = true"),
        "{text}"
    );
}

/// `three` plus, in `dirty`, a staged file and an untracked one.
fn three_messy(sb: &Sandbox) {
    three(sb);
    let d = sb.path("dev/dirty");
    write(&d, "staged", "s\n");
    git(&d, &["add", "staged"]);
    write(&d, "untracked", "u\n");
}

fn short_status(r: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = git(r, &["status", "--short"])
        .lines()
        .map(String::from)
        .collect();
    v.sort();
    v
}

#[test]
fn the_stash_fix_follows_the_stash_config() {
    let sb = Sandbox::new();
    three_messy(&sb);
    let d = sb.path("dev/dirty");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[stash]\nuntracked = false\nkeep_index = true\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["fix", "-k", "stash", "acme/dirty"])
        .assert()
        .success();
    assert_eq!(short_status(&d), ["?? untracked", "A  staged"]);
    // inverse: the defaults take everything
    git(&d, &["reset", "-q", "--hard"]);
    git(&d, &["stash", "pop", "-q", "--index"]);
    argus(&sb)
        .args(["fix", "-k", "stash", "acme/dirty"])
        .assert()
        .success();
    assert!(short_status(&d).is_empty(), "{:?}", short_status(&d));
}

/// What the completion engine offers for the command line `words` (as
/// typed so far, the last being completed), asked the way fish asks.
fn complete(sb: &Sandbox, config: &std::path::Path, words: &[&str]) -> Vec<String> {
    let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("argus"));
    for (k, v) in subprocess_env(&sb.root()) {
        c.env(k, v);
    }
    let out = c
        .env("COMPLETE", "fish")
        .env("ARGUS_CONFIG", config)
        .arg("--")
        .arg("argus")
        .args(words)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect()
}

#[test]
fn completion_offers_commands_options_and_repo_names() {
    let sb = Sandbox::new();
    three(&sb);
    sb.init("dev/local/thing");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, format!("roots = ['{}']\n", sb.path("dev").display())).unwrap();
    assert!(complete(&sb, &cfg, &["pu"]).contains(&"pull".to_string()));
    assert!(complete(&sb, &cfg, &["untag", "--a"]).contains(&"--all".to_string()));
    // tags, then repos
    assert_eq!(
        complete(&sb, &cfg, &["tag", ""])[..3],
        ["favourite", "pull", "ignore"]
    );
    assert_eq!(complete(&sb, &cfg, &["ignore-issue", "st"]), ["stashes"]);
    assert!(complete(&sb, &cfg, &["untag", "fa"]).contains(&"favourite".to_string()));
    // repo names, by owner/name or path, for every command taking patterns
    for cmd in [
        &["tag", "favourite"][..],
        &["untag", "pull"],
        &["snooze"],
        &["unsnooze"],
        &["ignore-issue", "stashes"],
        &["pull"],
        &["fix"],
        &["ack"],
    ] {
        let mut args = cmd.to_vec();
        args.push("acme/c");
        assert_eq!(complete(&sb, &cfg, &args), ["acme/clean"], "{cmd:?}");
    }
    let all = complete(&sb, &cfg, &["tag", "favourite", ""]);
    for name in ["acme/clean", "acme/dirty", "acme/feature"] {
        assert!(all.contains(&name.to_string()), "{all:?}");
    }
    assert!(
        all.iter().any(|n| n.ends_with("dev/local/thing")),
        "local repos by path: {all:?}"
    );
    // inverse: nothing matches, nothing offered
    assert!(complete(&sb, &cfg, &["tag", "favourite", "nomatch/"]).is_empty());
}

#[test]
fn a_config_error_is_reported_once_in_plain_words() {
    let sb = Sandbox::new();
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "concurrency = 3\n").unwrap();
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("status")
        .assert()
        .failure();
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert_eq!(err.matches("invalid type").count(), 1, "{err}");
    assert!(
        err.contains("[concurrency]") && !err.contains("struct"),
        "{err}"
    );
}

#[test]
fn root_add_and_remove_name_the_included_file_they_edit() {
    let sb = Sandbox::new();
    for d in ["dev", "work", "extra"] {
        std::fs::create_dir_all(sb.path(d)).unwrap();
    }
    let conf = sb.path(".config/argus");
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::write(conf.join("config.toml"), "extend = ['work.toml']\n").unwrap();
    std::fs::write(conf.join("work.toml"), "roots = ['~/work']\n").unwrap();
    let a = |args: &[&str]| argus_in(&sb, &sb.path("dev")).args(args).assert();
    a(&["root", "add", "~/extra"])
        .success()
        .stdout("added ~/extra (to ~/.config/argus/work.toml)\n");
    a(&["root", "add", "~/work"])
        .success()
        .stdout("~/work is already configured (in ~/.config/argus/work.toml)\n");
    a(&["root", "list"]).success().stdout("~/work\n~/extra\n");
    // listed in two files: both edited, both named
    std::fs::write(
        conf.join("config.toml"),
        "extend = ['work.toml']\nextend_roots = ['~/work']\n",
    )
    .unwrap();
    a(&["root", "remove", "~/work"])
        .success()
        .stdout("removed ~/work from ~/.config/argus/config.toml, ~/.config/argus/work.toml\n");
    a(&["root", "list"]).success().stdout("~/extra\n");
    assert_eq!(
        std::fs::read_to_string(conf.join("config.toml")).unwrap(),
        "extend = ['work.toml']\nextend_roots = []\n"
    );
}

#[test]
fn fix_leaves_an_ignored_state_alone() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        "[tags.ignore.issues]\nnon_default_branch = ['acme/feature@feat']\n",
    )
    .unwrap();
    let fix = |args: &[&str]| {
        argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .assert()
            .success()
    };
    fix(&["fix", "--dry-run"]).stdout(predicate::str::contains("would switch main").not());
    fix(&["fix"]).stdout(predicate::str::contains("nothing to fix"));
    let branch = git(&sb.path("dev/feature"), &["branch", "--show-current"]);
    assert_eq!(branch.trim(), "feat");
    // inverse: pinned to another branch, it's fixed as before
    std::fs::write(
        &cfg,
        "[tags.ignore.issues]\nnon_default_branch = ['acme/feature@other']\n",
    )
    .unwrap();
    fix(&["fix"]).stdout(predicate::str::contains("switch main"));
}

#[test]
fn issues_are_ignored_and_unignored() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "").unwrap();
    let run = |args: &[&str]| argus(&sb).arg("--config").arg(&cfg).args(args).assert();
    let read = || argus_core::Config::parse(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    run(&["ignore-issue", "non-default-branch", "acme/feature"]).success();
    assert_eq!(
        read().ignore_issues().non_default_branch,
        ["github.com/acme/feature@feat"]
    );
    // --no-pin and --pin choose the other way from the list's default
    run(&["unignore-issue", "non-default-branch", "acme/feature"]).success();
    run(&[
        "ignore-issue",
        "non-default-branch",
        "acme/feature",
        "--no-pin",
    ])
    .success();
    assert_eq!(
        read().ignore_issues().non_default_branch,
        ["github.com/acme/feature"]
    );
    run(&["unignore-issue", "non-default-branch", "acme/feature"]).success();
    let r = sb.path("dev/clean");
    write(&r, "README", "x\n");
    git(&r, &["stash", "push", "-q", "-m", "argus 2026-01-02 09:30"]);
    let mut stashes = read().ignore_issues().stashes.clone();
    run(&["ignore-issue", "stashes", "acme/clean"]).success();
    stashes.push("github.com/acme/clean".into());
    assert_eq!(read().ignore_issues().stashes, stashes);
    assert!(
        std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("extend_stashes"),
        "the default list is kept"
    );
    run(&["unignore-issue", "stashes", "acme/clean"]).success();
    run(&["ignore-issue", "stashes", "acme/clean", "--pin"]).success();
    *stashes.last_mut().unwrap() = "github.com/acme/clean@argus 2026-01-02 09:30".into();
    assert_eq!(read().ignore_issues().stashes, stashes);
    run(&["unignore-issue", "non-default-branch", "acme/feature"]).success();
    assert!(read().ignore_issues().non_default_branch.is_empty());
    // unknown or not ignorable: an error naming the ones that are
    run(&["ignore-issue", "dirty", "acme/dirty"])
        .failure()
        .stderr(predicate::str::contains("non-default-branch"));
}

#[test]
fn destructive_fixes_are_listed_and_need_yes() {
    let sb = Sandbox::new();
    three(&sb);
    let c = sb.path("dev/clean");
    git(&c, &["branch", "done"]);
    let has_done = || git_may_fail(&c, &["rev-parse", "--verify", "-q", "refs/heads/done"]);
    argus(&sb)
        .args(["fix", "-n", "-k", "delete-branches", "acme/clean"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "acme/clean: would delete branch done (done is merged into main), after asking",
        ));
    // without a terminal or --yes: refused, nothing deleted
    argus(&sb)
        .args(["fix", "-k", "delete-branches", "acme/clean"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("  acme/clean: delete branch done"))
        .stderr(predicate::str::contains("needs confirmation; pass --yes"));
    assert!(has_done());
    argus(&sb)
        .args(["fix", "-y", "-k", "delete-branches", "acme/clean"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("acme/clean: ")
                .and(predicate::str::contains("delete branch done")),
        );
    assert!(!has_done());
    // abort, alongside a safe kind that applies elsewhere
    git(&c, &["switch", "-q", "-c", "o"]);
    write(&c, "README", "o\n");
    commit(&c, "o");
    git(&c, &["switch", "-q", "main"]);
    write(&c, "README", "m\n");
    commit(&c, "m");
    assert!(!git_may_fail(&c, &["merge", "o"]));
    write(&sb.path("dev/dirty"), "README", "changed again\n");
    argus(&sb)
        .args(["fix", "--yes", "-k", "abort,stash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("abort merge"))
        .stdout(predicate::str::contains("acme/dirty: ").and(predicate::str::contains("stash")));
    assert!(!c.join(".git/MERGE_HEAD").exists());
    // nothing left to do
    argus(&sb)
        .args(["fix", "-y", "-k", "abort"])
        .assert()
        .success()
        .stdout("nothing to fix\n");
}

#[test]
fn ack_and_tag_reach_hidden_repos_and_ack_keeps_the_other_new_ones() {
    let sb = Sandbox::new();
    three(&sb);
    for n in ["n1", "n2"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    // as if argus had cloned both for a track
    let state = sb.path(".state/argus/state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(
        &state,
        "new = [\"github.com/acme/n1\", \"github.com/acme/n2\"]\n",
    )
    .unwrap();
    argus(&sb)
        .args(["snooze", "acme/n2", "--for", "2d"])
        .assert()
        .success();
    // a pattern reaches hidden repos too (a name always would)
    argus(&sb)
        .args(["tag", "favourite", "acme/n*"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/n2"));
    argus(&sb)
        .args(["ack", "acme/n1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acknowledged 1"));
    // snoozed, so hidden: acknowledging everything includes it
    let out = argus(&sb).args(["--json", "ack"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["acknowledged"], serde_json::json!(["acme/n2"]));
    argus(&sb)
        .arg("ack")
        .assert()
        .success()
        .stdout(predicate::str::contains("acknowledged 0"));
}

#[test]
fn plain_output_has_no_colour_codes() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .env_remove("NO_COLOR")
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b[").not());
}

#[test]
fn pull_takes_patterns_indents_failures_and_json_says_failed() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "tags.pull.repos = ['acme/clean', 'acme/feature']\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "--dry-run", "acme/feature"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would pull acme/feature"))
        .stdout(predicate::str::contains("acme/clean").not());
    // no progress line where stderr isn't a terminal
    for args in [
        &["pull", "acme/feature"][..],
        &["fix", "-n", "-F", "-k", "pull", "acme/feature"],
    ] {
        argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .assert()
            .success()
            .stderr(predicate::str::contains("\x1b[").not());
    }
    // a remote that's gone: the failure's output, indented under it
    let c = sb.path("dev/clean");
    git(
        &c,
        &[
            "config",
            "--remove-section",
            &format!("url.{}/", sb.path("forge").display()),
        ],
    );
    git(
        &c,
        &[
            "config",
            "url./nonexistent/.insteadOf",
            "https://github.com/",
        ],
    );
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "acme/clean"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    let after = stderr
        .split("failed: pull acme/clean\n")
        .nth(1)
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(after.starts_with("    ") && after.len() > 4, "{stderr}");
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["--json", "pull", "acme/clean"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let statuses: Vec<&str> = v["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["failed"], "{v}");
}

#[test]
fn tracks_ask_the_forge_unless_no_clone_and_forge_errors_are_reported() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[track]]\nowner = 'acme'\nhost = 'nothing.invalid'\ninto = '{}'\n",
            sb.path("dev").display()
        ),
    )
    .unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "--dry-run"])
        .assert()
        .stderr(predicate::str::contains("forge error"));
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "--dry-run", "--no-clone"])
        .assert()
        .success()
        .stderr(predicate::str::contains("forge error").not());
    argus(&sb)
        .args(["forge", "list", "--host", "nothing.invalid", "acme"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to lookup address"));
}

#[test]
fn the_tui_needs_a_terminal_before_anything_else() {
    let sb = Sandbox::new();
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "roots = 3\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a terminal"));
}

#[test]
fn glyph_columns_leave_two_spaces_after_the_widest_entry() {
    use unicode_width::UnicodeWidthStr;
    let sb = Sandbox::new();
    let out = argus(&sb).args(["demo", "--glyphs"]).output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    let header = lines.iter().position(|l| l.starts_with("icon ")).unwrap();
    // display column where `s` starts in `l`
    let col = |l: &str, s: &str| UnicodeWidthStr::width(&l[..l.find(s).unwrap()]);
    let first_set = col(lines[header], argus_core::icons::SETS[0]);
    let longest = argus_core::icons::ICON_NAMES
        .iter()
        .map(|n| n.len())
        .max()
        .unwrap();
    assert_eq!(first_set, longest + 2, "{}", lines[header]);
    // the first set's column: its widest cell, then two spaces
    let second = col(lines[header], argus_core::icons::SETS[1]);
    // its cells are "|g|MgM" and a marker ('*' when two cells wide in
    // CJK mode, else a space): the widest, then two spaces
    let set = argus_core::icons::by_name(argus_core::icons::SETS[0]);
    let widest = argus_core::icons::ICON_NAMES
        .iter()
        .map(|n| {
            let g = set.get(n).unwrap();
            UnicodeWidthStr::width(format!("|{g}|M{g}M ").as_str())
        })
        .chain([argus_core::icons::SETS[0].len()])
        .max()
        .unwrap();
    assert_eq!(second, first_set + widest + 2, "{}", lines[header]);
}

/// `forge/acme` with `have`, `fresh` and `gone`; `have` and `gone` cloned
/// from it into `dev/acme`, then `gone` removed from it unless `keep`.
fn dir_forge(sb: &Sandbox, keep: bool) -> std::path::PathBuf {
    for n in ["have", "fresh", "gone"] {
        sb.remote("acme", n);
    }
    for n in ["have", "gone"] {
        let from = sb.path(&format!("forge/acme/{n}.git"));
        let to = sb.path(&format!("dev/acme/{n}"));
        git(
            &sb.root(),
            &["clone", "-q", from.to_str().unwrap(), to.to_str().unwrap()],
        );
    }
    if !keep {
        std::fs::remove_dir_all(sb.path("forge/acme/gone.git")).unwrap();
    }
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[track]]\nforge = 'dir'\nhost = '{}'\nowner = 'acme'\n",
            sb.path("forge").display()
        ),
    )
    .unwrap();
    cfg
}

#[test]
fn pull_reports_what_a_directory_forge_has_and_no_longer_has() {
    let sb = Sandbox::new();
    let cfg = dir_forge(&sb, false);
    let mut notify = std::fs::read_to_string(&cfg).unwrap();
    notify.push_str("clone = 'notify'\n");
    std::fs::write(&cfg, notify).unwrap();
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["pull", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("new on forge: acme/fresh (not cloned)\n"),
        "{stdout}"
    );
    // (the sandbox is home)
    assert!(
        stdout.contains(
            "orphan: ~/dev/acme/gone: gone from the forge, or out of reach \
             (to hide it: argus tag ignore acme/gone)\n"
        ),
        "{stdout}"
    );
    assert!(
        !stdout.contains("acme/have ("),
        "listed and cloned: nothing to say\n{stdout}"
    );
    // inverse: JSON has its own fields, not these lines
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["--json", "pull", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains("new on forge") && !stdout.contains("orphan:"),
        "{stdout}"
    );
}

#[test]
fn pull_clones_what_a_directory_forge_has() {
    let sb = Sandbox::new();
    let cfg = dir_forge(&sb, true);
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("pull")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fresh = sb.path("dev/acme/fresh");
    assert!(fresh.join("README").exists(), "{stdout}");
    assert!(stdout.contains("cloned: ~/dev/acme/fresh\n"), "{stdout}");
    assert!(
        !stdout.contains("new on forge") && !stdout.contains("orphan:"),
        "{stdout}"
    );
}

#[test]
fn forge_list_reads_a_directory() {
    let sb = Sandbox::new();
    dir_forge(&sb, false);
    let forge = sb.path("forge");
    argus(&sb)
        .args([
            "forge",
            "list",
            "--forge",
            "dir",
            "--host",
            forge.to_str().unwrap(),
            "acme",
        ])
        .assert()
        .success()
        .stdout("acme/fresh\nacme/have\n");
}

#[test]
fn tag_owner_tags_the_repos_owners_as_a_whole() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "").unwrap();
    let run = |args: &[&str]| {
        argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .output()
            .unwrap()
    };
    let read = || argus_core::Config::parse(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(
        run(&["tag", "favourite", "acme/clean", "--owner"])
            .status
            .success()
    );
    assert_eq!(read().tags["favourite"].repos, ["github.com/acme/*"]);
    assert!(
        run(&["tag", "ignore", "acme/*", "--owner"])
            .status
            .success()
    );
    let c = read();
    assert!(
        c.tags["favourite"].repos.is_empty(),
        "ignore excludes favourite"
    );
    assert_eq!(c.tags["ignore"].repos, ["github.com/acme/*"]);
    assert!(
        run(&["untag", "ignore", "acme/*", "--owner"])
            .status
            .success()
    );
    assert!(read().tags["ignore"].repos.is_empty());
    // only for tagging and untagging one tag
    for args in [
        &["untag", "--all", "--owner", "acme/clean"][..],
        &["snooze", "--owner", "acme/clean", "--for", "1d"],
        &["ignore-issue", "behind", "acme/clean", "--owner"],
    ] {
        let o = run(args);
        assert!(!o.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("--owner"),
            "{args:?}"
        );
    }
}

/// `snooze` / `unsnooze` with `--issue` act on that issue only; plain
/// `unsnooze` ends the repo's issue snoozes too.
#[test]
fn snooze_one_issue() {
    let sb = Sandbox::new();
    three(&sb);
    let json = |args: &[&str]| -> serde_json::Value {
        let out = argus(&sb).args(args).arg("--json").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let v = json(&["snooze", "acme/dirty", "--for", "2d", "--issue", "dirty"]);
    assert!(v[0]["snoozed_issues"]["dirty"].is_string(), "{v}");
    assert!(v[0]["snoozed_until"].is_null(), "not the repo: {v}");
    // its only issue snoozed: off the default list, not hidden
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty").not())
        .stdout(predicate::str::contains("hidden (--hidden)").not());
    let v = json(&["unsnooze", "acme/dirty", "--issue", "dirty"]);
    assert!(
        v[0]["snoozed_issues"].as_object().unwrap().is_empty(),
        "{v}"
    );
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty"));
    json(&["snooze", "acme/dirty", "--for", "2d", "--issue", "dirty"]);
    let v = json(&["unsnooze", "acme/dirty"]);
    assert!(
        v[0]["snoozed_issues"].as_object().unwrap().is_empty(),
        "{v}"
    );

    argus(&sb)
        .args(["snooze", "acme/dirty", "--for", "2d", "--issue", "grumpy"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-default-branch"));
    argus(&sb)
        .args(["snooze", "acme/dirty", "--issue", "dirty"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--for"));
}

/// A tag of your own: `argus tag work …` / `argus untag work …` edit
/// `[tags.work] repos`; status shows its icon; an unknown tag is named
/// with the ones there are.
#[test]
fn tags_of_your_own_are_set_and_shown() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[tags.work]\nicon = 'W'\n").unwrap();
    let run = |args: &[&str]| {
        argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["tag", "work", "acme/*", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["tags"], serde_json::json!(["work"]), "{v}");
    assert!(run(&["untag", "work", "acme/dirty"]).status.success());
    let c = argus_core::Config::parse(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    // each repo by name (a pattern selects repos; `--owner` writes one)
    assert_eq!(
        c.tags["work"].repos,
        ["github.com/acme/clean", "github.com/acme/feature"]
    );
    assert_eq!(
        c.tags["work"].icon,
        Some(argus_core::config::TagIcon::One("W".into()))
    );
    let status = String::from_utf8(run(&["status", "--all"]).stdout).unwrap();
    let line = |n: &str| status.lines().find(|l| l.contains(n)).unwrap().to_string();
    assert!(
        line("acme/clean").trim_start().starts_with("W acme/clean"),
        "{status}"
    );
    assert!(
        line("acme/dirty").trim_start().starts_with("acme/dirty"),
        "{status}"
    );
    let o = run(&["tag", "home", "acme/clean"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("favourite, pull, ignore, work"), "{err}");
    // and one patterns don't name: nothing tagged
    let o = run(&["untag", "work"]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("which repos"));
}

/// `status --filter` takes `/`'s syntax (`#tag`, `!#tag`, `|`, fuzzy
/// text); `--tag` is shorthand for having each tag. An unknown tag is an
/// error, the ones there are named.
#[test]
fn status_filters_by_tags() {
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        "[tags.work]\nrepos = ['acme/clean', 'acme/dirty']\n[tags.home]\nrepos = ['acme/feature']\n",
    )
    .unwrap();
    let shown = |args: &[&str]| -> Vec<String> {
        let out = argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(["status", "--all"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        ["clean", "dirty", "feature"]
            .iter()
            .filter(|n| text.contains(&format!("acme/{n}")))
            .map(|n| n.to_string())
            .collect()
    };
    assert_eq!(shown(&["--filter", "#work"]), ["clean", "dirty"]);
    assert_eq!(shown(&["--filter", "!#work"]), ["feature"]);
    assert_eq!(shown(&["--filter", "#work | #home"]).len(), 3);
    // pull and ignore filter as tags do
    std::fs::write(
        &cfg,
        "[tags.work]\nrepos = ['acme/clean', 'acme/dirty']\n[tags.home]\nrepos = ['acme/feature']\n\
         [tags.pull]\nrepos = ['acme/dirty']\n[tags.ignore]\nrepos = ['acme/feature']\n",
    )
    .unwrap();
    assert_eq!(shown(&["--filter", "#pull"]), ["dirty"]);
    assert_eq!(shown(&["--tag", "pull"]), ["dirty"]);
    assert_eq!(shown(&["--hidden", "--filter", "#ignore"]), ["feature"]);
    assert_eq!(
        shown(&["--hidden", "--filter", "!#ignore !#pull"]),
        ["clean"]
    );
    std::fs::write(
        &cfg,
        "[tags.work]\nrepos = ['acme/clean', 'acme/dirty']\n[tags.home]\nrepos = ['acme/feature']\n",
    )
    .unwrap();
    assert_eq!(shown(&["--filter", "#work cln"]), ["clean"]);
    // text matches the key too: github.com/acme/…
    assert_eq!(shown(&["--filter", "#work gthb"]), ["clean", "dirty"]);
    assert_eq!(shown(&["--tag", "work", "--filter", "drt"]), ["dirty"]);
    assert_eq!(
        shown(&["--tag", "work", "--tag", "home"]),
        Vec::<String>::new()
    );
    let o = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["status", "--tag", "nope"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("favourite, pull, ignore, work, home"));
    // the footer says how many matched
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["status", "--all", "--tag", "home"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 matches the filter"));
}

/// `demo --glyphs` lists each tag's icon by set, and in yours, the set
/// your config chooses.
#[test]
fn glyphs_list_tags_by_set() {
    let sb = Sandbox::new();
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        "[ui]\nicons = 'ascii'\n[tags.work]\nicon = { unicode = 'W', ascii = 'w' }\n",
    )
    .unwrap();
    let out = argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["--json", "demo", "--glyphs"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["sets"]["unicode"]["tag.favourite"], "◆");
    assert_eq!(v["sets"]["yours"]["tag.favourite"], "*", "{v}");
    assert_eq!(v["sets"]["unicode"]["tag.work"], "W");
    assert_eq!(v["sets"]["yours"]["tag.work"], "w");
}

/// Completion follows `--root` and `--config` given earlier on the line:
/// repos from that root, tags from that config.
#[test]
fn completion_follows_root_and_config_on_the_line() {
    let sb = Sandbox::new();
    three(&sb);
    sb.remote("other", "elsewhere");
    sb.clone_as_github("other", "elsewhere", "far/elsewhere");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, format!("roots = ['{}']\n", sb.path("dev").display())).unwrap();
    let far = sb.path("far").display().to_string();
    assert!(
        complete(&sb, &cfg, &["ack", "other/"]).is_empty(),
        "not in the configured roots"
    );
    assert_eq!(
        complete(&sb, &cfg, &["--root", &far, "ack", "other/"]),
        ["other/elsewhere"]
    );
    assert_eq!(
        complete(&sb, &cfg, &[&format!("--root={far}"), "ack", "other/"]),
        ["other/elsewhere"]
    );
    assert!(
        complete(&sb, &cfg, &["--root", &far, "ack", "acme/"]).is_empty(),
        "the configured roots replaced, as when running"
    );
    let mine = sb.path("mine.toml");
    std::fs::write(&mine, "[tags.work]\n").unwrap();
    assert_eq!(
        complete(&sb, &cfg, &["--config", mine.to_str().unwrap(), "tag", "w"]),
        ["work"]
    );
    assert!(complete(&sb, &cfg, &["tag", "w"]).is_empty());
}

#[test]
fn status_details_prints_what_the_tui_details_show() {
    let sb = Sandbox::new();
    three(&sb);
    let out = argus(&sb)
        .args(["status", "--details"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    for want in [
        "    path      ",
        "    remote    origin ",
        "    issue     1 uncommitted path(s)",
        "    branches",
        "      * feat  origin/feat",
        // changes by section
        "\n    unstaged (1)\n        M README\n",
        // a fix the CLI can't run says where it can be
        "commit…  1 uncommitted path(s)  (in the TUI: c A)",
        // one it can names the command
        "(argus fix -k stash)",
    ] {
        assert!(out.contains(want), "{want:?} not in:\n{out}");
    }
    // the auto-fix is what plain `argus fix` runs: no command to name
    let auto = out.lines().find(|l| l.contains("auto-fix  ")).unwrap();
    assert!(
        auto.contains("switch") && !auto.contains("argus fix"),
        "{auto}"
    );
    // each repo's details follow its own row
    let row = |name: &str| out.find(name).unwrap();
    let readme = out.find("    M README").unwrap();
    assert!(
        row("acme/dirty") < readme && readme < row("acme/feature"),
        "{out}"
    );
    // inverse: no details without the flag, and never in JSON
    let plain = argus(&sb).arg("status").assert().success();
    plain.stdout(predicate::str::contains("issue     ").not());
    let with = argus(&sb)
        .args(["--json", "status", "--details"])
        .output()
        .unwrap();
    let without = argus(&sb).args(["--json", "status"]).output().unwrap();
    assert_eq!(with.stdout, without.stdout);
}

#[test]
fn status_details_says_where_describe_stopped() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    git(&r, &["tag", "v1"]);
    for m in ["a", "b", "c"] {
        commit(&r, m);
    }
    git(&r, &["switch", "-q", "--detach"]);
    let cfg = sb.path("argus.toml");
    let details = |limit: usize| {
        std::fs::write(&cfg, format!("describe_limit = {limit}\n")).unwrap();
        let o = argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(["status", "--all", "--details"])
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap()
    };
    let out = details(2);
    assert!(
        out.contains("describe  no tag in the last 2 commits"),
        "{out}"
    );
    assert!(!out.contains("searching"), "{out}");
    assert!(details(1).contains("no tag in the last commit\n"));
    // inverse: found within the limit, it's shown and nothing else is said
    let out = details(100);
    assert!(
        out.contains("describe  v1-3-g") && !out.contains("no tag in"),
        "{out}"
    );
}

#[test]
fn fix_auto_keeps_fixing_until_nothing_more_applies() {
    let sb = Sandbox::new();
    three(&sb);
    let f = sb.path("dev/feature");
    commit(&f, "unpushed");
    let o = argus(&sb)
        .args(["fix", "--auto", "acme/feature"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(o.stdout).unwrap(),
        "acme/feature: ✓ push\nacme/feature: ✓ switch main\n"
    );
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "main");
    // nothing left: said once, not looped on
    argus(&sb)
        .args(["fix", "--auto", "acme/feature"])
        .assert()
        .success()
        .stdout("nothing to fix\n");
}

#[test]
fn fix_auto_stops_at_a_fix_that_fails() {
    let sb = Sandbox::new();
    three(&sb);
    let f = sb.path("dev/feature");
    commit(&f, "unpushed");
    let gone = sb.path("nowhere.git");
    git(
        &f,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            gone.to_str().unwrap(),
        ],
    );
    let o = argus(&sb)
        .args(["fix", "--auto", "acme/feature"])
        .output()
        .unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    // tried once, and nothing after it
    assert_eq!(out.matches("push").count(), 1, "{out}");
    assert!(out.starts_with("acme/feature: ✗ push\n"), "{out}");
    assert!(!out.contains("switch"), "{out}");
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "feat");
}

/// A stand-in for GitHub's API on a local port: answers every request
/// with `body`, and gives back the requests it got.
fn fake_api(body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    fake_api_routes(vec![("", body.to_string())])
}

/// The same, answering by path: the first route whose path the request's
/// starts with, else 404.
fn fake_api_routes(
    routes: Vec<(&'static str, String)>,
) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            // headers, then as much body as Content-Length says
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap_or(0);
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                let Some((head, sent)) = text.split_once("\r\n\r\n") else {
                    if n == 0 {
                        break;
                    }
                    continue;
                };
                let want = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if sent.len() >= want || n == 0 {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&got).to_string();
            let path = request.split(' ').nth(1).unwrap_or_default().to_string();
            let _ = tx.send(request);
            let (status, body) = match routes.iter().find(|(p, _)| path.starts_with(p)) {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", r#"{"message":"Not Found"}"#.to_string()),
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, rx)
}

#[test]
fn status_forge_asks_the_forge_and_shows_pull_requests_and_issues() {
    let sb = Sandbox::new();
    three(&sb);
    let body = r#"{"data":{"repository":{"issues":{"totalCount":2},"pullRequests":{"totalCount":1},
        "b0":{"nodes":[{"number":7,"title":"Add feat","state":"OPEN","url":"https://github.com/acme/feature/pull/7",
          "headRepositoryOwner":{"login":"acme"},"reviewDecision":"APPROVED"}]},"parent":null}}}"#;
    let (base, requests) = fake_api(body);
    let run = |args: &[&str]| {
        let o = argus(&sb)
            .env("ARGUS_GITHUB_API", &base)
            .env("GH_TOKEN", "tok")
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    // it brings the details with it
    let out = run(&["status", "--forge", "--filter", "feature"]);
    assert!(
        out.contains("    forge     2 open issues, 1 open pull request\n"),
        "{out}"
    );
    assert!(
        out.contains("      * feat  origin/feat  #7 open, approved  Add feat\n"),
        "{out}"
    );
    // (links only on a terminal: none in a pipe)
    assert!(!out.contains("\x1b]8"), "{out}");
    // one question for the one repo listed, with the token, about its branch
    let asked: Vec<String> = requests.try_iter().collect();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with("POST /graphql "), "{}", asked[0]);
    assert!(
        asked[0]
            .to_lowercase()
            .contains("authorization: bearer tok")
    );
    assert!(asked[0].contains(r#""b0":"feat""#) && asked[0].contains(r#""name":"feature""#));
    // inverse: nothing is asked without the flag, details or not
    let out = run(&["status", "--details"]);
    assert!(!out.contains("forge     "), "{out}");
    assert_eq!(requests.try_iter().count(), 0);
    // nor when turned off in config
    let cfg = sb.path("off.toml");
    std::fs::write(&cfg, "ui.forge_info = false\n").unwrap();
    let out = run(&["--config", cfg.to_str().unwrap(), "status", "--forge"]);
    assert!(
        out.contains("path      ") && !out.contains("forge     "),
        "{out}"
    );
    assert_eq!(requests.try_iter().count(), 0);
}

#[test]
fn status_forge_says_why_a_forge_did_not_answer() {
    let sb = Sandbox::new();
    three(&sb);
    let (base, _requests) = fake_api(r#"{"errors":[{"message":"Something went wrong"}]}"#);
    let o = argus(&sb)
        .env("ARGUS_GITHUB_API", &base)
        .env("GH_TOKEN", "tok")
        .args(["status", "--forge", "--filter", "dirty"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(
        out.contains("    forge     Something went wrong\n"),
        "{out}"
    );
    // and it isn't in --json yet: said, not silently left out
    argus(&sb)
        .args(["--json", "status", "--forge"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--forge"));
}

fn api_repo(owner: &str, name: &str) -> String {
    format!(
        r#"{{"name":"{name}","owner":{{"login":"{owner}"}},"ssh_url":"git@github.com:{owner}/{name}.git","clone_url":"https://github.com/{owner}/{name}.git"}}"#
    )
}

/// `acme` tracked, only `clean` pulled; the forge lists only `clean`, has
/// `dirty` under a new name and knows nothing of `feature`.
fn orphaned(sb: &Sandbox) -> (std::path::PathBuf, String) {
    three(sb);
    let cfg = sb.path("cfg.toml");
    std::fs::write(
        &cfg,
        "[tags.pull]\nrepos = ['!acme/dirty', '!acme/feature']\n[[track]]\nowner = 'acme'\nclone = 'notify'\n",
    )
    .unwrap();
    let (base, _requests) = fake_api_routes(vec![
        (
            "/orgs/acme/repos",
            format!("[{}]", api_repo("acme", "clean")),
        ),
        ("/repos/acme/dirty", api_repo("other", "fresh")),
    ]);
    (cfg, base)
}

#[test]
fn pull_says_what_became_of_orphans() {
    let sb = Sandbox::new();
    let (cfg, base) = orphaned(&sb);
    let run = |args: &[&str]| {
        argus(&sb)
            .env("ARGUS_GITHUB_API", &base)
            .env("GH_TOKEN", "tok")
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .output()
            .unwrap()
    };
    let url = || git(&sb.path("dev/dirty"), &["config", "remote.origin.url"]);
    let before = url();
    let o = run(&["pull"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    // renamed: where to, and how to follow it (nothing is done unasked)
    assert!(
        out.contains(
            "orphan: ~/dev/dirty: now other/fresh (argus pull --yes points its remote there)\n"
        ),
        "{out}"
    );
    // gone: the clone may be the only copy, so only hiding it is offered
    assert!(
        out.contains("orphan: ~/dev/feature: gone from the forge, or out of reach (to hide it: argus tag ignore acme/feature)\n"),
        "{out}"
    );
    assert!(!out.contains("clean"), "{out}");
    assert_eq!(url(), before, "not followed without being asked");
    // a dry run says what --yes would do, and does nothing
    let out = String::from_utf8(run(&["--yes", "pull", "-n"]).stdout).unwrap();
    assert!(
        out.contains("would follow acme/dirty to other/fresh\n"),
        "{out}"
    );
    assert_eq!(url(), before);
    // as JSON: each orphan, what became of it and what it is now
    let v: serde_json::Value =
        serde_json::from_slice(&run(&["--json", "pull", "-n"]).stdout).unwrap();
    assert_eq!(
        v["orphans"],
        serde_json::json!([
            {"path": "~/dev/dirty", "repo": "acme/dirty", "found": "now other/fresh", "now": "other/fresh"},
            {"path": "~/dev/feature", "repo": "acme/feature", "found": "gone from the forge, or out of reach", "now": null},
        ])
    );
    // --yes follows the rename: the remote, otherwise as it was
    let o = run(&["--yes", "pull"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("follow acme/dirty to other/fresh"), "{out}");
    // only that is said to be followed: not the pull that also ran, and
    // no hint to do what was just done
    assert!(out.contains("2 jobs: 2 ok"), "{out}");
    assert!(!out.contains("clean"), "{out}");
    assert!(!out.contains("points its remote there"), "{out}");
    assert_eq!(
        url().trim(),
        before.trim().replace("acme/dirty", "other/fresh")
    );
    // the gone one is still there, untouched
    assert!(sb.path("dev/feature/.git").exists());
}

#[test]
fn status_names_line_up_whatever_flags_the_rows_have() {
    use unicode_width::UnicodeWidthStr;
    let sb = Sandbox::new();
    three(&sb);
    let cfg = sb.path("cfg.toml");
    let columns = |config: &str| -> Vec<usize> {
        std::fs::write(&cfg, config).unwrap();
        let o = argus(&sb)
            .arg("--config")
            .arg(&cfg)
            .args(["status", "--all"])
            .output()
            .unwrap();
        let out = String::from_utf8(o.stdout).unwrap();
        out.lines()
            .filter_map(|l| l.find("acme/").map(|at| l[..at].width()))
            .collect()
    };
    // a nerd icon carries its own space; one row has two flags, one none
    let c = columns(
        "[ui]\nicons = 'nerd'\n[tags.favourite]\nrepos = ['acme/clean', 'acme/dirty']\n\
         [tags.work]\nicon = 'W'\nrepos = ['acme/dirty']\n",
    );
    assert_eq!(c.len(), 3);
    assert!(c.iter().all(|w| *w == c[0]), "{c:?}");
    // as wide as the most flags and a space: two cells of star, one of W
    assert_eq!(c[0], 4);
    // inverse: with no flags anywhere, just the space
    assert_eq!(columns(""), [1, 1, 1]);
    // and three tags on one row still line up
    let c = columns(
        "[tags.a]\nrepos = ['acme/clean']\n[tags.b]\nrepos = ['acme/clean']\n\
         [tags.c]\nrepos = ['acme/clean', 'acme/feature']\n",
    );
    assert_eq!(c, [4, 4, 4]);
}

#[test]
fn text_from_a_repo_never_reaches_the_terminal_as_escape_sequences() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    // a file name, a branch-like stash message and a commit subject, each
    // with a sequence that would retitle the window or clear the screen
    write(&r, "evil\x1b]0;pwned\x07.txt", "x\n");
    write(&r, "README", "stash me\n");
    git(&r, &["stash", "push", "-q", "-m", "stash\x1b[2J"]);
    git(
        &r,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "subject\x1b[31m red\x1b]52;c;AAAA\x07",
        ],
    );
    for args in [
        &["status", "--all", "--details"][..],
        &["fix", "-n"][..],
        &["pull", "-n"][..],
    ] {
        let o = argus(&sb).args(args).output().unwrap();
        for (what, bytes) in [("stdout", &o.stdout), ("stderr", &o.stderr)] {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains('\x1b') && !text.contains('\x07'),
                "{args:?} {what}: {text:?}"
            );
        }
    }
    // the name is still shown, with the odd characters marked
    let o = argus(&sb).args(["status", "--details"]).output().unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains("evil\u{fffd}]0;pwned\u{fffd}.txt"), "{out}");
    assert!(out.contains("stash\u{fffd}[2J"), "{out}");
}

/// The squash-merge check's answers are kept in the cache for the next
/// run; `argus paths` says where, and keeps nothing itself.
#[test]
fn squash_merge_answers_are_kept_in_the_cache() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    write(&r, "f", "feature\n");
    commit(&r, "feat");
    git(&r, &["switch", "-q", "main"]);
    git(&r, &["merge", "-q", "--squash", "feat"]);
    git(&r, &["commit", "-q", "-m", "feat (#1)"]);
    git(&r, &["push", "-q"]);
    let file = sb.root().join(".cache/argus/absorbed");
    let paths = argus(&sb).arg("paths").output().unwrap();
    let out = String::from_utf8(paths.stdout).unwrap();
    assert!(
        out.contains(&format!("cache:  {}", file.display())),
        "{out}"
    );
    assert!(!file.exists(), "paths keeps nothing");
    argus(&sb).args(["status", "-a"]).assert().success();
    let id = |rev: &str| git(&r, &["rev-parse", rev]).trim().to_string();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{} {} 1\n", id("feat"), id("origin/main"))
    );
}
