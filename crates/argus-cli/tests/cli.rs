//! End-to-end CLI tests: the real binary against sandbox repositories.

use argus_core::testkit::{Sandbox, commit, git, subprocess_env, write};
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
        .stdout(predicate::str::contains("acme/dirty").and(predicate::str::contains("✚1")))
        .stdout(predicate::str::contains("acme/clean").not())
        .stdout(predicate::str::contains(
            "2 of 3 repos need attention; 1 clean not shown (--all)",
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
        .stdout("0 of 1 repos need attention; 1 clean not shown (--all)\n");
}

/// argus with an isolated environment but no `--root`, run from `cwd`.
fn argus_in(sb: &Sandbox, cwd: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("argus").unwrap();
    for (k, v) in subprocess_env(&sb.root()) {
        c.env(k, v);
    }
    c.env_remove("ARGUS_CONFIG")
        .env_remove("ARGUS_STATE")
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
    a(&["root", "add", "../work", "."]).success().stdout(
        predicate::str::contains("added ~/work").and(predicate::str::contains("~/dev").not()),
    );
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
        .failure()
        .stderr(predicate::str::contains("not configured as a root"));
    a(&["root", "list"]).success().stdout("~/dev\n");
    // the generated documentation survived the edits
    let text = std::fs::read_to_string(sb.path(".config/argus/config.toml")).unwrap();
    assert!(
        text.contains("## Maximum directory depth below a root"),
        "{text}"
    );
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
        .stdout(predicate::str::contains("1 repos, 1 need attention"))
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

#[test]
fn fix_with_explicit_kind_and_pattern() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .args(["fix", "-k", "stash", "acme/clean"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to fix"));
    argus(&sb)
        .args(["fix", "-k", "stash", "acme/dirty"])
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ stash"));
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
    // an unknown kind is rejected
    argus(&sb)
        .args(["fix", "-n", "-k", "push,shove"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value 'shove'"));
}

#[test]
fn mark_snooze_and_ack_roundtrip_through_state() {
    let sb = Sandbox::new();
    three(&sb);
    // favourites lead their group, but unclean repos still come first
    argus(&sb)
        .args(["mark", "acme/clean", "acme/feature", "--favourite"])
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
        lines[0].contains("★") && lines[2].contains("★"),
        "{lines:?}"
    );

    argus(&sb)
        .args(["mark", "acme/dirty", "--snooze", "2d"])
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
        .args(["mark", "acme/dirty", "--unsnooze"])
        .assert()
        .success();
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty"));

    argus(&sb)
        .args(["mark", "acme/*", "--snooze", "soon"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad duration"));
    argus(&sb)
        .args(["mark", "nomatch/*", "--exclude"])
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
fn sync_pulls_syncable_and_reports_failures_with_exit_code() {
    let sb = Sandbox::new();
    three(&sb);
    sb.advance_remote("acme", "clean", "upstream-file");
    let cfg = sb.path("cfg.toml");
    std::fs::write(&cfg, "[[rule]]\nmatch = 'acme/clean'\nsync = true\n").unwrap();
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .args(["sync", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would pull acme/clean"));
    assert!(!sb.path("dev/clean/upstream-file").exists());
    argus(&sb)
        .arg("--config")
        .arg(&cfg)
        .arg("sync")
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
        .arg("sync")
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
        &["sync"],
        &["fix"],
        &["mark"],
        &["ack"],
        &["forge", "list"],
        &["root", "add"],
        &["root", "remove"],
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
    // default [fix] auto = push, switch-default: stash is only an alternative
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
        "acme/w: would pull (diverged from origin/feat: 1 ahead, 1 behind; merges cleanly)\n  \
         then: push (1 commit(s) ahead of origin/feat)\n"
    );
}
