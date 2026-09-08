#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

const MISSING: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn run_ostree(args: &[&str]) -> Output {
    Command::new("ostree")
        .args(args)
        .output()
        .expect("run ostree")
}

fn must(args: &[&str]) {
    let output = run_ostree(args);
    assert!(
        output.status.success(),
        "ostree {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn binary(repo: &Path, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ostree-missing-refs"))
        .arg(repo)
        .args(args)
        .output()
        .expect("run repair tool")
}

fn binary_with_env(repo: &Path, args: &[String], key: &str, value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ostree-missing-refs"))
        .arg(repo)
        .args(args)
        .env(key, value)
        .output()
        .expect("run repair tool")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.stderr.is_empty(),
        "tracing must use stdout: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn assert_event(output: &Output, event: &str) {
    assert!(
        stdout(output).contains(&format!("audit_event=\"{event}\"")),
        "stdout={}",
        stdout(output)
    );
}

fn repo_arg(repo: &Path) -> String {
    format!("--repo={}", repo.display())
}

fn write_ref(repo: &Path, reference: &str, checksum: &str) {
    let path = repo.join("refs/heads").join(reference);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("{checksum}\n")).unwrap();
}

fn rev(repo: &Path, reference: &str) -> String {
    let repo_arg = repo_arg(repo);
    let output = run_ostree(&[&repo_arg, "rev-parse", reference]);
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    fallback_checksum: String,
    healthy: String,
    healthy_checksum: String,
    summary: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("repo");
        let repo_arg = repo_arg(&repo);
        must(&[&repo_arg, "init", "--mode=archive"]);
        let tree = temp.path().join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("payload"), "fallback\n").unwrap();
        let fallback = "fedora/45/x86_64/test";
        must(&[
            &repo_arg,
            "commit",
            &format!("--branch={fallback}"),
            &format!("--tree=dir={}", tree.display()),
            "--subject=fallback",
        ]);
        let fallback_checksum = rev(&repo, fallback);
        let healthy = "fedora/rawhide/x86_64/healthy".to_owned();
        fs::write(tree.join("payload"), "healthy distinct\n").unwrap();
        must(&[
            &repo_arg,
            "commit",
            &format!("--branch={healthy}"),
            &format!("--tree=dir={}", tree.display()),
            "--subject=healthy",
        ]);
        let healthy_checksum = rev(&repo, &healthy);
        must(&[&repo_arg, "summary", "--update"]);
        let summary = fs::read(repo.join("summary")).unwrap();
        Self {
            temp,
            repo,
            fallback_checksum,
            healthy,
            healthy_checksum,
            summary,
        }
    }
}

#[test]
fn help_and_usage_use_stdout_and_conventional_statuses() {
    let help = Command::new(env!("CARGO_BIN_EXE_ostree-missing-refs"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert_event(&help, "help");
    for args in [vec![], vec!["repo", "--unknown"], vec!["repo", "--expect"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_ostree-missing-refs"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "args={args:?}");
        assert_event(&output, "usage_error");
    }
}

#[test]
fn dry_run_and_apply_preserve_safe_refs_and_summary() {
    let f = Fixture::new();
    let broken = "fedora/rawhide/x86_64/test";
    let absent = "fedora/rawhide/x86_64/absent";
    write_ref(&f.repo, broken, MISSING);
    write_ref(&f.repo, absent, MISSING);
    let dry = binary(&f.repo, &[]);
    assert_eq!(dry.status.code(), Some(1));
    assert_event(&dry, "repair_candidate");
    assert_eq!(rev(&f.repo, broken), MISSING);

    let apply = binary(&f.repo, &["--apply".into()]);
    assert_eq!(apply.status.code(), Some(3));
    assert_event(&apply, "transaction_committed");
    assert_event(&apply, "ref_verified");
    assert_eq!(rev(&f.repo, broken), f.fallback_checksum);
    assert_eq!(rev(&f.repo, &f.healthy), f.healthy_checksum);
    assert_eq!(rev(&f.repo, absent), MISSING);
    assert_eq!(fs::read(f.repo.join("summary")).unwrap(), f.summary);
}

#[test]
fn corrupt_source_is_repaired_but_corrupt_fallback_is_not_selected() {
    let f = Fixture::new();
    let raw = "fedora/rawhide/x86_64/corrupt";
    let repo_arg = repo_arg(&f.repo);
    let tree = f.temp.path().join("corrupt-tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("payload"), "corrupt\n").unwrap();
    must(&[
        &repo_arg,
        "commit",
        &format!("--branch={raw}"),
        &format!("--tree=dir={}", tree.display()),
        "--subject=corrupt",
    ]);
    let old = rev(&f.repo, raw);
    write_ref(&f.repo, "fedora/45/x86_64/corrupt", &f.fallback_checksum);
    let object = f
        .repo
        .join("objects")
        .join(&old[..2])
        .join(format!("{}.commit", &old[2..]));
    let mut bytes = fs::read(&object).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    fs::write(&object, bytes).unwrap();
    let apply = binary(&f.repo, &["--apply".into()]);
    assert_eq!(apply.status.code(), Some(0));
    assert_eq!(rev(&f.repo, raw), f.fallback_checksum);

    let f = Fixture::new();
    write_ref(&f.repo, "fedora/rawhide/x86_64/test", MISSING);
    let object = f
        .repo
        .join("objects")
        .join(&f.fallback_checksum[..2])
        .join(format!("{}.commit", &f.fallback_checksum[2..]));
    let mut bytes = fs::read(&object).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    fs::write(&object, bytes).unwrap();
    let dry = binary(&f.repo, &[]);
    assert_eq!(dry.status.code(), Some(1));
    assert_event(&dry, "candidate_skipped");
    assert_eq!(rev(&f.repo, "fedora/rawhide/x86_64/test"), MISSING);
}

#[test]
fn automatic_summary_and_non_archive_are_refused() {
    let f = Fixture::new();
    let broken = "fedora/rawhide/x86_64/test";
    write_ref(&f.repo, broken, MISSING);
    for key in ["core.auto-update-summary", "core.commit-update-summary"] {
        let configured = Fixture::new();
        write_ref(&configured.repo, broken, MISSING);
        must(&[&repo_arg(&configured.repo), "config", "set", key, "true"]);
        let summary = binary(&configured.repo, &["--apply".into()]);
        assert_eq!(summary.status.code(), Some(2), "key={key}");
        assert_eq!(rev(&configured.repo, broken), MISSING, "key={key}");
        assert_eq!(
            fs::read(configured.repo.join("summary")).unwrap(),
            configured.summary
        );
    }
    let bare = f.temp.path().join("bare");
    must(&[&repo_arg(&bare), "init", "--mode=bare-user"]);
    let non_archive = binary(&bare, &[]);
    assert_eq!(non_archive.status.code(), Some(2));
    assert_event(&non_archive, "preflight_failed");
}

#[test]
fn malformed_refs_refuse_all_updates() {
    let f = Fixture::new();
    let broken = "fedora/rawhide/x86_64/test";
    write_ref(&f.repo, broken, MISSING);
    write_ref(&f.repo, "fedora/rawhide/x86_64/malformed", "not-a-checksum");
    let malformed = binary(&f.repo, &[]);
    assert_eq!(malformed.status.code(), Some(2));
    assert_event(&malformed, "malformed_ref");
    assert_eq!(rev(&f.repo, broken), MISSING);
}

#[test]
fn metadata_io_errors_and_remote_refs_are_not_repaired() {
    let f = Fixture::new();
    let raw = "fedora/rawhide/x86_64/io-error";
    let repo_arg = repo_arg(&f.repo);
    let tree = f.temp.path().join("io-error-tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("payload"), "io error\n").unwrap();
    must(&[
        &repo_arg,
        "commit",
        &format!("--branch={raw}"),
        &format!("--tree=dir={}", tree.display()),
        "--subject=io-error",
    ]);
    let old = rev(&f.repo, raw);
    let object = f
        .repo
        .join("objects")
        .join(&old[..2])
        .join(format!("{}.commit", &old[2..]));
    fs::remove_file(&object).unwrap();
    fs::create_dir(&object).unwrap();
    let output = binary(&f.repo, &[]);
    assert_eq!(output.status.code(), Some(2));
    assert_event(&output, "discovery_failed");
    assert_eq!(rev(&f.repo, raw), old);

    let f = Fixture::new();
    write_ref(
        &f.repo,
        "../remotes/origin/fedora/rawhide/x86_64/remote-only",
        MISSING,
    );
    let output = binary(&f.repo, &[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(!stdout(&output).contains("remote-only"));
}

#[test]
fn multiple_selected_refs_are_staged_and_verified_together() {
    let f = Fixture::new();
    let first = "fedora/rawhide/x86_64/test";
    let second = "fedora/rawhide/x86_64/second";
    write_ref(&f.repo, first, MISSING);
    write_ref(&f.repo, second, MISSING);
    write_ref(&f.repo, "fedora/45/x86_64/second", &f.fallback_checksum);
    let output = binary(&f.repo, &["--apply".into()]);
    assert_eq!(output.status.code(), Some(0));
    assert_event(&output, "transaction_committed");
    assert_event(&output, "apply_finished");
    let events = stdout(&output);
    assert_eq!(events.matches("audit_event=\"ref_verified\"").count(), 2);
    assert!(
        events.find("audit_event=\"transaction_committed\"")
            < events.find("audit_event=\"ref_verified\"")
            && events.find("audit_event=\"ref_verified\"")
                < events.find("audit_event=\"apply_finished\"")
    );
    assert_eq!(rev(&f.repo, first), f.fallback_checksum);
    assert_eq!(rev(&f.repo, second), f.fallback_checksum);
}

#[test]
fn pre_commit_failure_does_not_verify_or_update_refs() {
    let f = Fixture::new();
    let broken = "fedora/rawhide/x86_64/test";
    write_ref(&f.repo, broken, MISSING);
    let output = binary_with_env(
        &f.repo,
        &["--apply".into()],
        "OSTREE_REPO_TEST_ERROR",
        "pre-commit",
    );
    assert_eq!(output.status.code(), Some(3));
    assert_event(&output, "transaction_commit_failed");
    assert!(!stdout(&output).contains("audit_event=\"ref_verified\""));
    assert_eq!(rev(&f.repo, broken), MISSING);
}
