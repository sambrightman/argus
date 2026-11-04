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
    argus(&sb)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/dirty").and(predicate::str::contains("✚1")))
        .stdout(predicate::str::contains("3 repos, 2 need attention"))
        .stdout(predicate::str::contains("\x1b[").not());

    let out = argus(&sb)
        .args(["status", "--json", "--attention"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["dirty", "feature"]);
}

#[test]
fn no_roots_is_a_helpful_error() {
    let sb = Sandbox::new();
    let mut c = Command::cargo_bin("argus").unwrap();
    for (k, v) in subprocess_env(&sb.root()) {
        c.env(k, v);
    }
    c.arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("argus init"));
}

#[test]
fn init_writes_config_once() {
    let sb = Sandbox::new();
    argus(&sb).arg("init").assert().success();
    assert!(sb.path(".config/argus/config.toml").exists());
    argus(&sb)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    argus(&sb).args(["init", "--force"]).assert().success();
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
}

#[test]
fn mark_snooze_and_ack_roundtrip_through_state() {
    let sb = Sandbox::new();
    three(&sb);
    argus(&sb)
        .args(["mark", "acme/clean", "--favourite"])
        .assert()
        .success();
    let out = argus(&sb).arg("status").output().unwrap();
    let first = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(
        first.contains("★") && first.contains("acme/clean"),
        "{first}"
    );

    argus(&sb)
        .args(["mark", "acme/dirty", "--snooze", "2d"])
        .assert()
        .success();
    argus(&sb)
        .arg("status")
        .assert()
        .stdout(predicate::str::contains("acme/dirty").not());
    argus(&sb)
        .args(["status", "--all"])
        .assert()
        .stdout(predicate::str::contains("acme/dirty"));
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
