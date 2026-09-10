#![forbid(unsafe_code)]
//! Repair missing or corrupt Fedora Rawhide refs without rewriting healthy refs.

mod logs;

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ostree::{ObjectType, Repo, RepoListRefsExtFlags, RepoMode, gio};
use tracing::{error, info, warn};

const RAW_PREFIX: &str = "fedora/rawhide/";
const FALLBACK_PREFIX: &str = "fedora/45/";
const EXIT_BROKEN: u8 = 1;
const EXIT_ERROR: u8 = 2;
const EXIT_PARTIAL: u8 = 3;

#[derive(Clone)]
struct StdoutWriter {
    stdout: Arc<io::Stdout>,
    failed: Arc<AtomicBool>,
}

impl StdoutWriter {
    fn new() -> Self {
        Self {
            stdout: Arc::new(io::stdout()),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

struct RecordingWriter<W> {
    inner: W,
    failed: Arc<AtomicBool>,
}

impl<W: Write> Write for RecordingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.inner.write(buffer) {
            Ok(written) => Ok(written),
            Err(_) => {
                self.failed.store(true, Ordering::Relaxed);
                // tracing cannot propagate writer errors to main. Record the
                // failure and consume this event so it does not emit a second
                // diagnostic on stderr; main returns EXIT_ERROR below.
                Ok(buffer.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.inner.flush().is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for StdoutWriter {
    type Writer = RecordingWriter<io::StdoutLock<'a>>;

    fn make_writer(&'a self) -> Self::Writer {
        RecordingWriter {
            inner: self.stdout.lock(),
            failed: Arc::clone(&self.failed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    raw_ref: String,
    old: String,
    fallback_ref: String,
    new: String,
}

#[derive(Default)]
struct Args {
    repo: PathBuf,
    apply: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Clean,
    Broken,
    Applied,
    Partial,
    Error,
}

impl Outcome {
    fn exit_code(self) -> ExitCode {
        match self {
            Self::Clean | Self::Applied => ExitCode::SUCCESS,
            Self::Broken => ExitCode::from(EXIT_BROKEN),
            Self::Error => ExitCode::from(EXIT_ERROR),
            Self::Partial => ExitCode::from(EXIT_PARTIAL),
        }
    }
}

fn usage(message: &str) -> String {
    format!(
        "{message}\nusage: ostree-missing-refs REPOSITORY [--apply]\n       ostree-missing-refs logs [--compose COMPOSE_ID]"
    )
}

fn checksum(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_lowercase() && c.is_ascii_hexdigit()))
}

fn fallback_ref(raw: &str) -> Option<String> {
    raw.strip_prefix(RAW_PREFIX)
        .filter(|suffix| !suffix.is_empty())
        .map(|suffix| format!("{FALLBACK_PREFIX}{suffix}"))
}

enum Command {
    Repair(Args),
    Logs(logs::Args),
}

fn parse_args() -> Result<Command, String> {
    parse_values(env::args_os().skip(1))
}

fn parse_values(mut values: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let first = values
        .next()
        .ok_or_else(|| usage("missing repository argument"))?;
    if first == "logs" {
        let mut logs = logs::Args::default();
        while let Some(value) = values.next() {
            match value.to_str() {
                Some("--compose") => {
                    let compose = values
                        .next()
                        .and_then(|value| value.into_string().ok())
                        .ok_or_else(|| usage("--compose requires a compose ID"))?;
                    if logs.compose.replace(compose).is_some() {
                        return Err(usage("--compose specified more than once"));
                    }
                }
                Some("--help") | Some("-h") => return Err(usage("")),
                _ => return Err(usage("unknown logs option")),
            }
        }
        return Ok(Command::Logs(logs));
    }
    let mut args = Args {
        repo: PathBuf::from(first),
        ..Args::default()
    };
    for value in values {
        match value.to_str() {
            Some("--apply") => args.apply = true,
            Some("--help") | Some("-h") => return Err(usage("")),
            _ => return Err(usage("unknown option")),
        }
    }
    Ok(Command::Repair(args))
}

fn refs(repo: &Repo, prefix: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    repo.list_refs_ext(
        prefix,
        RepoListRefsExtFlags::EXCLUDE_REMOTES | RepoListRefsExtFlags::EXCLUDE_MIRRORS,
        None::<&gio::Cancellable>,
    )
    .map(|r| r.into_iter().collect())
    .map_err(|e| e.to_string())
}

fn exact_ref(repo: &Repo, reference: &str) -> Result<Option<String>, String> {
    Ok(refs(repo, Some(reference))?.remove(reference))
}

/// Only absence and libostree's checksum-mismatch/not-found diagnostics are corruption.
fn integrity(repo: &Repo, value: &str) -> Result<Result<(), String>, String> {
    if !repo
        .has_object(ObjectType::Commit, value, None::<&gio::Cancellable>)
        .map_err(|e| e.to_string())?
    {
        return Ok(Err("commit metadata object is absent".to_owned()));
    }
    match repo.fsck_object(ObjectType::Commit, value, None::<&gio::Cancellable>) {
        Ok(()) => Ok(Ok(())),
        Err(error) => {
            let message = error.to_string();
            if (error.matches(gio::IOErrorEnum::Failed)
                && message.contains("Corrupted commit object; checksum expected="))
                || (error.matches(gio::IOErrorEnum::NotFound)
                    && message.contains("No such metadata object"))
            {
                Ok(Err(message))
            } else {
                Err(message)
            }
        }
    }
}

fn ensure_summary_is_not_automatic(repo: &Repo) -> Result<(), String> {
    let config = repo.config();
    for key in ["auto-update-summary", "commit-update-summary"] {
        match config.boolean("core", key) {
            Ok(true) => {
                return Err(format!(
                    "core.{key}=true would regenerate the summary; refusing"
                ));
            }
            Ok(false) => {}
            Err(error)
                if error.matches(glib::KeyFileError::KeyNotFound)
                    || error.matches(glib::KeyFileError::GroupNotFound) => {}
            Err(error) => return Err(format!("cannot read core.{key}: {error}")),
        }
    }
    Ok(())
}

fn discover(repo: &Repo) -> Result<(Vec<Candidate>, usize), String> {
    let all = refs(repo, None)?;
    let mut candidates = Vec::new();
    let mut skipped = 0;
    for (raw, old) in all.iter().filter(|(name, _)| name.starts_with(RAW_PREFIX)) {
        if !checksum(old) {
            error!(audit_event = "malformed_ref", raw_ref = %raw, old_checksum = %old, "refusing malformed Rawhide ref");
            return Err("repository contains malformed Rawhide refs".into());
        }
        if integrity(repo, old)?.is_ok() {
            continue;
        }
        let fallback = fallback_ref(raw).expect("Rawhide prefix has a non-empty suffix");
        let Some(new) = all.get(&fallback) else {
            skipped += 1;
            warn!(audit_event = "candidate_skipped", raw_ref = %raw, old_checksum = %old, fallback_ref = %fallback, reason = "fallback is absent");
            continue;
        };
        if !checksum(new) {
            skipped += 1;
            warn!(audit_event = "candidate_skipped", raw_ref = %raw, old_checksum = %old, fallback_ref = %fallback, reason = "fallback has invalid checksum");
            continue;
        }
        if let Err(reason) = integrity(repo, new)? {
            skipped += 1;
            warn!(audit_event = "candidate_skipped", raw_ref = %raw, old_checksum = %old, fallback_ref = %fallback, new_checksum = %new, %reason, "fallback is unavailable");
            continue;
        }
        info!(audit_event = "repair_candidate", raw_ref = %raw, old_checksum = %old, fallback_ref = %fallback, new_checksum = %new);
        candidates.push(Candidate {
            raw_ref: raw.clone(),
            old: old.clone(),
            fallback_ref: fallback,
            new: new.clone(),
        });
    }
    Ok((candidates, skipped))
}

fn recheck_candidate(repo: &Repo, candidate: &Candidate) -> Result<(), String> {
    if exact_ref(repo, &candidate.raw_ref)? != Some(candidate.old.clone()) {
        return Err("source changed since detection".into());
    }
    if exact_ref(repo, &candidate.fallback_ref)? != Some(candidate.new.clone()) {
        return Err("fallback changed since detection".into());
    }
    if integrity(repo, &candidate.old)?.is_ok() {
        return Err("source became healthy since detection".into());
    }
    if let Err(reason) = integrity(repo, &candidate.new)? {
        return Err(format!("fallback is no longer available: {reason}"));
    }
    Ok(())
}

fn abort_transaction(repo: &Repo, cause: &str) -> Outcome {
    match repo.abort_transaction(None::<&gio::Cancellable>) {
        Ok(()) => {
            error!(audit_event = "transaction_aborted", %cause);
            Outcome::Error
        }
        Err(error) => {
            error!(audit_event = "transaction_abort_failed", %cause, error = %error);
            Outcome::Partial
        }
    }
}

fn apply(repo: &Repo, candidates: Vec<Candidate>, skipped: usize) -> Outcome {
    warn!(
        audit_event = "concurrency_warning",
        "libostree transactions do not exclusively serialize ref writers; stop all repository writers during apply"
    );
    if let Err(error) = ensure_summary_is_not_automatic(repo) {
        error!(audit_event = "preflight_failed", %error);
        return Outcome::Error;
    }
    for candidate in &candidates {
        info!(audit_event = "repair_selected", raw_ref = %candidate.raw_ref, old_checksum = %candidate.old, new_checksum = %candidate.new);
    }
    info!(
        audit_event = "transaction_prepare",
        selected = candidates.len()
    );
    if let Err(error) = repo.prepare_transaction(None::<&gio::Cancellable>) {
        error!(audit_event = "transaction_prepare_failed", error = %error);
        return Outcome::Error;
    }
    for candidate in &candidates {
        if let Err(error) = recheck_candidate(repo, candidate) {
            error!(audit_event = "precondition_failed", raw_ref = %candidate.raw_ref, %error);
            return abort_transaction(repo, &error);
        }
    }
    for candidate in &candidates {
        repo.transaction_set_ref(None, &candidate.raw_ref, Some(&candidate.new));
        info!(audit_event = "ref_staged", raw_ref = %candidate.raw_ref, old_checksum = %candidate.old, new_checksum = %candidate.new);
    }
    if let Err(error) = repo.commit_transaction(None::<&gio::Cancellable>) {
        error!(audit_event = "transaction_commit_failed", error = %error);
        if let Err(abort_error) = repo.abort_transaction(None::<&gio::Cancellable>) {
            error!(audit_event = "transaction_abort_failed", error = %abort_error);
        } else {
            info!(
                audit_event = "transaction_aborted",
                reason = "commit failed"
            );
        }
        return Outcome::Partial;
    }
    info!(
        audit_event = "transaction_committed",
        staged = candidates.len(),
        skipped,
        "commit returned successfully"
    );
    for candidate in &candidates {
        match exact_ref(repo, &candidate.raw_ref) {
            Ok(Some(value)) if value == candidate.new => match integrity(repo, &candidate.new) {
                Ok(Ok(())) => {
                    info!(audit_event = "ref_verified", raw_ref = %candidate.raw_ref, new_checksum = %candidate.new)
                }
                Ok(Err(error)) | Err(error) => {
                    error!(audit_event = "verification_failed", raw_ref = %candidate.raw_ref, %error);
                    info!(
                        audit_event = "apply_finished",
                        outcome = "partial",
                        repaired = candidates.len(),
                        skipped
                    );
                    return Outcome::Partial;
                }
            },
            Ok(observed) => {
                error!(audit_event = "verification_failed", raw_ref = %candidate.raw_ref, ?observed);
                info!(
                    audit_event = "apply_finished",
                    outcome = "partial",
                    repaired = candidates.len(),
                    skipped
                );
                return Outcome::Partial;
            }
            Err(error) => {
                error!(audit_event = "verification_failed", raw_ref = %candidate.raw_ref, %error);
                info!(
                    audit_event = "apply_finished",
                    outcome = "partial",
                    repaired = candidates.len(),
                    skipped
                );
                return Outcome::Partial;
            }
        }
    }
    info!(
        audit_event = "apply_finished",
        repaired = candidates.len(),
        skipped,
        outcome = if skipped == 0 { "applied" } else { "partial" }
    );
    if skipped == 0 {
        Outcome::Applied
    } else {
        Outcome::Partial
    }
}

fn run(args: Args) -> Outcome {
    let path = match fs::canonicalize(&args.repo) {
        Ok(path) => path,
        Err(error) => {
            error!(audit_event = "preflight_failed", repository = %args.repo.display(), error = %error, "cannot resolve repository");
            return Outcome::Error;
        }
    };
    info!(audit_event = "run_started", repository = %path.display(), mode = if args.apply { "apply" } else { "dry-run" });
    let repo = Repo::new(&gio::File::for_path(&path));
    if let Err(error) = repo.open(None::<&gio::Cancellable>) {
        error!(audit_event = "preflight_failed", repository = %path.display(), error = %error, "cannot open OSTree repository");
        return Outcome::Error;
    }
    if repo.mode() != RepoMode::Archive {
        error!(audit_event = "preflight_failed", mode = ?repo.mode(), "refusing non-archive repository");
        return Outcome::Error;
    }
    let (candidates, skipped) = match discover(&repo) {
        Ok(value) => value,
        Err(error) => {
            error!(audit_event = "discovery_failed", %error);
            return Outcome::Error;
        }
    };
    let outcome = if args.apply {
        apply(&repo, candidates, skipped)
    } else if candidates.is_empty() && skipped == 0 {
        Outcome::Clean
    } else {
        Outcome::Broken
    };
    info!(audit_event = "run_finished", outcome = ?outcome);
    outcome
}

fn main() -> ExitCode {
    let writer = StdoutWriter::new();
    tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .init();
    if env::args_os()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        info!(
            audit_event = "help",
            "usage: ostree-missing-refs REPOSITORY [--apply]; ostree-missing-refs logs [--compose COMPOSE_ID]"
        );
        return if writer.failed() {
            ExitCode::from(EXIT_ERROR)
        } else {
            ExitCode::SUCCESS
        };
    }
    let outcome = match parse_args() {
        Ok(Command::Repair(args)) => run(args).exit_code(),
        Ok(Command::Logs(args)) => match logs::run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => {
                error!(audit_event = "logs_failed", path = %failure.path.display(), error = %failure.message, "compose logs retained for inspection");
                ExitCode::from(EXIT_ERROR)
            }
        },
        Err(message) => {
            error!(audit_event = "usage_error", %message);
            ExitCode::from(EXIT_ERROR)
        }
    };
    if writer.failed() {
        ExitCode::from(EXIT_ERROR)
    } else {
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_lowercase_sha256() {
        assert!(checksum(&"a1".repeat(32)));
        assert!(!checksum(&"A1".repeat(32)));
        assert!(!checksum("bad"));
    }

    #[test]
    fn maps_only_rawhide_suffixes() {
        assert_eq!(
            fallback_ref("fedora/rawhide/x86_64/test"),
            Some("fedora/45/x86_64/test".into())
        );
        assert_eq!(fallback_ref("fedora/rawhide/"), None);
        assert_eq!(fallback_ref("fedora/45/x"), None);
    }

    #[test]
    fn recording_writer_reports_write_and_flush_failures() {
        struct Fails;
        impl Write for Fails {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("full"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("full"))
            }
        }

        let failed = Arc::new(AtomicBool::new(false));
        let mut writer = RecordingWriter {
            inner: Fails,
            failed: Arc::clone(&failed),
        };
        assert!(writer.write_all(b"event").is_ok());
        assert!(failed.load(Ordering::Relaxed));
        failed.store(false, Ordering::Relaxed);
        assert!(writer.flush().is_ok());
        assert!(failed.load(Ordering::Relaxed));
    }

    #[test]
    fn dispatches_logs_without_a_repository() {
        let command = parse_values(
            ["logs", "--compose", "Fedora-Rawhide-20260826.n.0"]
                .into_iter()
                .map(OsString::from),
        )
        .unwrap();
        let Command::Logs(args) = command else {
            panic!("logs subcommand was not dispatched")
        };
        assert_eq!(args.compose.as_deref(), Some("Fedora-Rawhide-20260826.n.0"));
    }

    #[test]
    fn treats_dot_slash_logs_as_a_repair_repository_and_rejects_logs_apply() {
        let repair = parse_values(["./logs"].into_iter().map(OsString::from)).unwrap();
        let Command::Repair(args) = repair else {
            panic!("./logs must remain a repository argument")
        };
        assert_eq!(args.repo, PathBuf::from("./logs"));
        assert!(parse_values(["logs", "--apply"].into_iter().map(OsString::from)).is_err());
    }
}
