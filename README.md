# ostree-missing-refs

Repair broken Fedora Rawhide OSTree refs by resetting them to their Fedora 45
equivalents. A ref is eligible only when its commit metadata object is missing
or corrupt and the corresponding Fedora 45 commit is valid.

The tool uses safe libostree transaction bindings for integrity checks and ref updates.
It does not delete objects or regenerate the repository summary. Healthy refs
are never changed.

## Background

This tool addresses [Fedora Release Engineering issue #13509: Rawhide OSTree
and OSTree-based disk image builds failing due to corrupt/missing
commits](https://forge.fedoraproject.org/releng/tickets/issues/13509).
The issue reports failures starting with the 2026-08-23 Rawhide compose:
rpm-ostree could not load the previous commit's SELinux policy because its
commit metadata was missing, and image builds received HTTP 404 errors when
fetching commit objects from the compose repository. This blocked Atomic
Desktop images and updates for users tracking Rawhide; IoT composes were
reported to be unaffected.

The discussion suggests a lost commit left refs pointing to nonexistent
objects, but does not establish the root cause. The proposed recovery is to
reset affected refs to known-existing commits rather than wipe the repository.
This tool implements a narrowly scoped version of that recovery: it offers
the matching `fedora/45/` ref as the fallback for each broken
`fedora/rawhide/` ref and validates the fallback commit metadata before
applying it.
It does not recover lost objects or fix the underlying cause.

## Build

```sh
cargo build --release
```

This requires Rust 1.85 or newer and the libostree development files. The test
suite additionally requires the `ostree` command-line tool.

## Use

First run without `--apply` to inspect the repair plan:

```sh
ostree-missing-refs /path/to/repo
```

Every invocation emits human-readable `tracing` events to stdout. The default
is read-only: review the candidate events, then use `--apply` to repair every
eligible candidate in one transaction:

```sh
ostree-missing-refs /path/to/repo --apply
```

There is no JSON report or durable audit file; stdout tracing is the
operational audit stream.

Apply rechecks every selected checksum after its transaction starts. However,
the safe transaction API does not exclusively serialize ref writers, and its
commit can be interrupted between ref updates. Stop **all** repository writers
(including cooperating compose, prune, and ref-writing tools) for the entire
apply operation. Direct filesystem writers and synchronization tools must also
remain stopped. The post-prepare rechecks reduce, but cannot eliminate, this
remaining concurrent-writer race.

Apply refuses repositories configured with `core.auto-update-summary=true` or
`core.commit-update-summary=true` so that ref updates cannot rewrite `summary`.

## Exit status

Exit status is 0 for a clean scan or successful apply, 1 when dry-run finds
broken refs, 2 for usage, preflight, or operational errors, and 3 for a partial
or uncertain apply. `--help` exits successfully.

## Test

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The integration tests create real temporary OSTree repositories, including refs
that point to nonexistent or cryptographically corrupt commit objects.
