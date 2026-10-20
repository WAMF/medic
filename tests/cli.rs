//! End-to-end tests: build throwaway git repositories and run the binary.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("repo");
        fs::create_dir(&dir).unwrap();
        let repo = Repo { _tmp: tmp, dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.dir, args)
    }

    fn write(&self, path: &str, body: &str) {
        let p = self.dir.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn branch(&self, name: &str, from: &str) {
        self.git(&["checkout", "-q", "-b", name, from]);
    }

    fn medic(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_medic"))
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend(args);
        let out = self.medic(&all);
        assert!(
            out.status.success(),
            "medic {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn lines(n: usize, tag: &str) -> String {
    (1..=n).map(|i| format!("{tag} line {i}\n")).collect()
}

fn replace_line(text: &str, line: usize, with: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            if i + 1 == line {
                with.to_string()
            } else {
                l.to_string()
            }
        })
        .map(|l| l + "\n")
        .collect()
}

fn labels(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

fn pr<'a>(report: &'a Value, label: &str) -> &'a Value {
    report["prs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["label"] == label)
        .unwrap()
}

fn pair<'a>(report: &'a Value, a: &str, b: &str) -> Option<&'a Value> {
    report["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| (p["a"] == a && p["b"] == b) || (p["a"] == b && p["b"] == a))
}

/// A base with a library file, a helper and a README, plus a set of PR
/// branches that exercise overlap, conflict, rename and three-dot cases.
fn triage_repo() -> Repo {
    let r = Repo::new();
    let lib = lines(30, "lib");
    r.write("src/lib.rs", &lib);
    r.write("src/util.rs", &lines(30, "util"));
    r.write("README.md", "# demo\n");
    r.commit("initial");

    r.branch("pr-a", "main");
    r.write("src/lib.rs", &replace_line(&lib, 1, "a changes line 1"));
    r.commit("a");

    r.branch("pr-b", "main");
    r.write("src/lib.rs", &replace_line(&lib, 1, "b changes line 1"));
    r.commit("b");

    r.branch("pr-c", "main");
    r.write("src/lib.rs", &replace_line(&lib, 25, "c changes line 25"));
    r.commit("c");

    r.branch("pr-docs", "main");
    r.write("README.md", "# demo\n\nMore words.\n");
    r.commit("docs");

    r.branch("pr-rename", "main");
    r.git(&["mv", "src/util.rs", "src/helpers.rs"]);
    r.commit("rename util");

    r.branch("pr-util", "main");
    let util = lines(30, "util");
    r.write("src/util.rs", &replace_line(&util, 10, "util edit"));
    r.commit("edit util");

    r.branch("pr-stale", "main");
    r.write("src/lib.rs", &replace_line(&lib, 15, "stale edit of 15"));
    r.commit("stale");

    // main moves on after the branches were cut.
    r.git(&["checkout", "-q", "main"]);
    r.write("src/main_only.rs", "fn main() {}\n");
    r.write("src/lib.rs", &replace_line(&lib, 15, "main edit of 15"));
    r.commit("main moves");
    r
}

#[test]
fn overlap_lists_three_dot_files_and_renames() {
    let r = triage_repo();
    let report = r.json(&["overlap", "--base", "main", "pr-a", "pr-rename", "pr-util"]);

    let a = pr(&report, "pr-a");
    let files: Vec<&str> = a["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    // Three-dot: main's own later commit is not part of pr-a.
    assert_eq!(files, vec!["src/lib.rs"]);

    let rename = &pr(&report, "pr-rename")["files"][0];
    assert_eq!(rename["status"], "R");
    assert_eq!(rename["old_path"], "src/util.rs");
    assert_eq!(rename["path"], "src/helpers.rs");

    // The rename and the edit share the old path but merge cleanly.
    let p = pair(&report, "pr-rename", "pr-util").expect("rename pair");
    assert_eq!(labels(&p["shared_files"]), vec!["src/util.rs"]);
    assert_eq!(p["conflict"], false);
}

#[test]
fn overlap_finds_conflicts_and_suggests_clean_set() {
    let r = triage_repo();
    let head_before = r.git(&["rev-parse", "HEAD"]);
    let report = r.json(&[
        "overlap", "--base", "main", "pr-a", "pr-b", "pr-c", "pr-docs", "pr-stale",
    ]);

    let ab = pair(&report, "pr-a", "pr-b").expect("a-b pair");
    assert_eq!(ab["conflict"], true);
    assert_eq!(labels(&ab["conflict_files"]), vec!["src/lib.rs"]);

    let ac = pair(&report, "pr-a", "pr-c").expect("a-c pair");
    assert_eq!(ac["conflict"], false);
    assert_eq!(labels(&ac["shared_files"]), vec!["src/lib.rs"]);

    assert!(pair(&report, "pr-a", "pr-docs").is_none());

    let stale = pr(&report, "pr-stale");
    assert_eq!(stale["base_conflict"], true);
    assert_eq!(stale["risk"]["level"], "medium");

    let order = labels(&report["suggestion"]["merge_order"]);
    assert!(order.contains(&"pr-docs".to_string()));
    assert!(order.contains(&"pr-c".to_string()));
    assert!(!order.contains(&"pr-stale".to_string()));
    // Exactly one of the two conflicting changes is taken.
    assert_eq!(
        order
            .iter()
            .filter(|l| *l == "pr-a" || *l == "pr-b")
            .count(),
        1
    );
    let excluded: Vec<&str> = report["suggestion"]["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["label"].as_str().unwrap())
        .collect();
    assert_eq!(excluded.len(), 2);

    // Nothing in the work tree, index or HEAD changed.
    assert_eq!(r.git(&["status", "--porcelain"]), "");
    assert_eq!(r.git(&["rev-parse", "HEAD"]), head_before);
}

#[test]
fn avoid_overlap_keeps_shared_files_apart() {
    let r = triage_repo();
    let report = r.json(&[
        "overlap",
        "--avoid-overlap",
        "--base",
        "main",
        "pr-a",
        "pr-c",
    ]);
    assert_eq!(labels(&report["suggestion"]["merge_order"]).len(), 1);
}

#[test]
fn overlap_text_output() {
    let r = triage_repo();
    let out = r.medic(&["overlap", "--files", "--base", "main", "pr-a", "pr-b"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Conflicts between changes (1):"), "{text}");
    assert!(text.contains("pr-a x pr-b: src/lib.rs"), "{text}");
    assert!(text.contains("M src/lib.rs"), "{text}");
    assert!(text.contains("Suggested merge set (1 of 2):"), "{text}");
}

#[test]
fn risk_docs_only_is_low() {
    let r = triage_repo();
    let v = r.json(&["risk", "--base", "main", "pr-docs"]);
    assert_eq!(v["risk"]["level"], "low");
    assert_eq!(v["risk"]["docs_only"], true);
}

#[test]
fn risk_large_spread_change_is_high() {
    let r = triage_repo();
    r.branch("pr-big", "main");
    for m in 0..12 {
        for f in 0..5 {
            r.write(&format!("mod{m}/src/file{f}.rs"), &lines(30, "code"));
        }
    }
    r.write(".github/workflows/ci.yml", "on: push\n");
    r.commit("big change");
    let v = r.json(&["risk", "--base", "main", "pr-big"]);
    assert_eq!(v["risk"]["level"], "high", "{:#}", v["risk"]["reasons"]);
    let reasons = v["risk"]["reasons"].to_string();
    assert!(reasons.contains("size"), "{reasons}");
    assert!(reasons.contains("ci"), "{reasons}");
}

#[test]
fn risk_conflict_with_other_change_raises_score() {
    let r = triage_repo();
    let alone = r.json(&["risk", "--base", "main", "pr-a"]);
    let against = r.json(&["risk", "--base", "main", "pr-a", "--against", "pr-b"]);
    let s1 = alone["risk"]["score"].as_f64().unwrap();
    let s2 = against["risk"]["score"].as_f64().unwrap();
    assert!(s2 >= s1 + 20.0, "{s1} -> {s2}");
    assert!(against["risk"]["reasons"]
        .to_string()
        .contains("conflicts with pr-b"));
}

#[test]
fn risk_fail_on_sets_exit_code() {
    let r = triage_repo();
    let out = r.medic(&["risk", "--base", "main", "pr-stale", "--fail-on", "medium"]);
    assert_eq!(out.status.code(), Some(3));
    let out = r.medic(&["risk", "--base", "main", "pr-docs", "--fail-on", "medium"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn risk_weights_come_from_config_file() {
    let r = triage_repo();
    let base = r.json(&["risk", "--base", "main", "pr-stale"]);
    assert_eq!(base["risk"]["level"], "medium");
    // A repo-root .medic.toml is picked up without --config.
    r.write(".medic.toml", "[conflict]\nbase = 0\n");
    let v = r.json(&["risk", "--base", "main", "pr-stale"]);
    assert_eq!(v["risk"]["level"], "low");
    assert!(v["config"].as_str().unwrap().ends_with(".medic.toml"));
    // An explicit --config wins.
    let cfg = r.dir.join("strict.toml");
    fs::write(&cfg, "[levels]\nmedium = 0\nhigh = 1\n").unwrap();
    let v = r.json(&[
        "--config",
        cfg.to_str().unwrap(),
        "risk",
        "--base",
        "main",
        "pr-docs",
    ]);
    assert_eq!(v["risk"]["level"], "high");
}

#[test]
fn bad_config_is_an_error() {
    let r = triage_repo();
    r.write(".medic.toml", "[size]\nweigth = 3\n");
    let out = r.medic(&["risk", "--base", "main", "pr-a"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid config"));
}

#[test]
fn config_prints_defaults() {
    let r = triage_repo();
    let out = r.medic(&["config", "--defaults"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("[size]"));
    assert!(text.contains("[paths]"));
}

#[test]
fn unknown_ref_is_a_runtime_error() {
    let r = triage_repo();
    let out = r.medic(&["risk", "--base", "main", "no-such-branch"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-branch"));
}

#[test]
fn usage_error_exits_2() {
    let r = triage_repo();
    let out = r.medic(&["overlap", "--base", "main"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn fetch_pull_request_refs_from_remote() {
    let upstream = triage_repo();
    // Publish two branches as GitHub-style pull request refs.
    upstream.git(&["update-ref", "refs/pull/1/head", "pr-a"]);
    upstream.git(&["update-ref", "refs/pull/2/head", "pr-b"]);
    let clone = upstream._tmp.path().join("clone");
    git_in(
        upstream._tmp.path(),
        &[
            "clone",
            "-q",
            "--single-branch",
            "--branch",
            "main",
            upstream.dir.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let out = Command::new(env!("CARGO_BIN_EXE_medic"))
        .args([
            "-C",
            clone.to_str().unwrap(),
            "--json",
            "overlap",
            "--fetch",
            "origin",
            "--base",
            "main",
            "1",
            "#2",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["base"]["ref"], "refs/remotes/origin/main");
    assert_eq!(report["prs"][0]["label"], "#1");
    assert_eq!(report["prs"][0]["ref"], "refs/medic/pull/1");
    assert_eq!(pair(&report, "#1", "#2").unwrap()["conflict"], true);

    // A later run without --fetch finds the fetched PR refs by number.
    let out = Command::new(env!("CARGO_BIN_EXE_medic"))
        .args([
            "-C",
            clone.to_str().unwrap(),
            "--json",
            "risk",
            "--base",
            "origin/main",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
