# Incident notes: Fedora releng #13509

_Research updated 2026-09-11._

## Current published repository

This is a subset failure, not every Rawhide ref. On 2026-09-09, recursively
parsing the Apache indexes below
[`refs/heads/fedora/rawhide/`](https://kojipkgs.fedoraproject.org/compose/ostree/repo/refs/heads/fedora/rawhide/)
found 12 refs (five `aarch64`, seven `x86_64`). Each small ref file was fetched
in full to obtain its checksum. Its pointed-to `.commit` object was then
requested with HTTP range `0-0`; the object responses below are `206` for
present objects and `404` for absent ones. This checks publication-side object
existence only, **not** commit checksum, signature, metadata, or
referenced-tree integrity.

| Ref | Value | Published `.commit` status |
| --- | --- | --- |
| `aarch64/atomic-host` | `61574049646f0f8e7c87683a02a01b60ec58b76a2d6b725bc28060bc12c79f53` | 206 (present) |
| `aarch64/cosmic-atomic` | `420ccd7b70ecefbe99d77b0eaa1d60b905cc8cfed71e1d0c2424dbee8e20311b` | 404 (absent) |
| `aarch64/kinoite` | `a423d78ea2dac4c1d4ec2706d31d41800638a15db84585d329acce2a1f66c484` | 404 (absent) |
| `aarch64/sericea` | `d2b412b8fbbd2bd609a9bac7d0fe61710eb5e4b91729d9b6411543bc8443e091` | 206 (present) |
| `aarch64/silverblue` | `c16ab41270ef38870d16bc8ba89aaad761ff61fa46e0e06a4b107692993196f1` | 404 (absent) |
| `x86_64/atomic-host` | `d283922f4157ffb01cdd2aac2e1eefd7c5bc9d4aee7061066154d3b69e2ac67b` | 206 (present) |
| `x86_64/cosmic-atomic` | `36483ec755aa84957d03ef144b135b41e1f9f9275081c2886f597c6410534645` | 404 (absent) |
| `x86_64/kinoite` | `018794c826e658d9dd5cbe6a10eb982a84377197c2f61b5e3b326ac5dc98ab21` | 404 (absent) |
| `x86_64/onyx` | `b544c1bb4084412e7537876970ad8183176d730e4985c84eeecc944eb15d8c04` | 206 (present) |
| `x86_64/sericea` | `83a5179bade3379eaf6ca7aba3f782f89af76156bd0275edb0cf30035b681366` | 206 (present) |
| `x86_64/silverblue` | `64abaa037f4b7036be191b6f7d27eb892d9f9b071fc31eb4121fd1dcd23847cf` | 404 (absent) |
| `x86_64/workstation` | `043bdb5911fccb5048a7a4510cd08339c42d9f1bf5b2c7766cbe5896b82d70ba` | 206 (present) |

Thus, on 2026-09-09, 6/12 published refs pointed at unavailable commit
metadata objects: both architectures of COSMIC-Atomic, Kinoite, and
Silverblue. This remains a historical publication-side observation, not an
origin comparison. In [the ticket](https://forge.fedoraproject.org/releng/tickets/issues/13509),
Kevin's 2026-09-10 repair dry run identified six origin repair candidates, but
did not determine whether they were missing or corrupt.

## Captured compose evidence

The collector retained complete captures:

- `/var/tmp/ostree-missing-refs-logs-J2w5Ac`: `Fedora-Rawhide-20260826.n.0`,
  13 files / 812,012 bytes.
- `/var/tmp/ostree-missing-refs-logs-fbfAxV`: `Fedora-Rawhide-20260909.n.0`,
  15 files / 821,909 bytes.

While the composes remain retained, collect them with `ostree-missing-refs logs --compose
Fedora-Rawhide-20260826.n.0` and `ostree-missing-refs logs --compose
Fedora-Rawhide-20260909.n.0` (or `ostree-missing-refs logs` to pin current
`latest-Fedora-Rawhide/COMPOSE_ID`). Their source base is
[`compose/rawhide/`](https://kojipkgs.fedoraproject.org/compose/rawhide/).
By September 11, the public index starts at August 28, so the August 26
capture above is local historical evidence, not currently downloadable there.

The August capture has five `create-ostree-repo.log` failures, with the same
five previous checksums as the corresponding September failures. Each ends in
`Loading previous sepolicy: No such metadata object <checksum>.commit`; for
example, `logs/x86_64/Kinoite/ostree-3/create-ostree-repo.log` in the retained
August capture.
COSMIC-Atomic x86_64 is not a sixth such log: it failed earlier while copying
the config Git checkout (August global log lines 3235--3280).

September 9 has all six missing-parent logs, including
[COSMIC-Atomic aarch64](https://kojipkgs.fedoraproject.org/compose/rawhide/Fedora-Rawhide-20260909.n.0/logs/aarch64/COSMIC-Atomic/ostree-2/create-ostree-repo.log).
September 11 has five logs with the same five hashes; COSMIC-Atomic x86_64
again failed earlier during the config-Git copy ([global log](https://kojipkgs.fedoraproject.org/compose/rawhide/Fedora-Rawhide-20260911.n.0/logs/global/pungi.global.log),
lines 3208--3253). This failure is not proof that no repair ever occurred, and
there is no evidence here that the proposed repair was applied.

The parsed August 26 and September 9 config dumps set `tag_ref: false` for all
five OSTree variants. They otherwise show `atomic-desktops/config.git` branch
`main`, the shared `/mnt/koji/compose/ostree/repo/`, `force_new_commit: true`,
`unified_core: true`, and `selinux-policy-targeted`; the latest `extra_config`
also confirms five available jobs. The collected create logs report
`rpm-ostree 2026.2`, and both global logs report
`pungi-4.13.0-1.fc44.noarch`. September 11's global process still reports 4.13,
but its [Silverblue x86_64 runroot package log](https://kojipkgs.fedoraproject.org/work/tasks/2174/150122174/root.log)
records `pungi-4.14.0-2.fc46`, `rpm-ostree-2026.2-4.fc46`, and
`ostree-2026.4-1.fc46`. The runroot package performs the tree composition;
its identity must not be inferred from the global Pungi version. The upstream
comparison below does not establish the content of Fedora RPM patches.
These version observations are correlation, not cause determination.

Other compose failures, including config-Git copy, image-builder, and KIWI
failures, do not establish missing OSTree objects. The `runroot.log`
`BuildrootError: rm exited with status 1` summaries likewise are not distinct
root causes when their adjacent create logs identify the OSTree error.

## Focused Pungi instrumentation proposal

This is a proposal only; no Pungi or Fedora RPM source change is claimed. The
reviewed source is [Pungi HEAD 2265c1375c81cfd9ca2d60743479ea0d9592d2c4](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4).
The dereferenced 4.13 and 4.14 tags are respectively
`fefe8704bf2259a8fc0aed9f388856e49a2d6978` and
`67dfc3417958c801592f0f4cceb0710c9eebd009`;
[tree.py](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/ostree/tree.py),
[ostree.py](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/phases/ostree.py),
and [runroot.py](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/runroot.py)
are unchanged between them. No conclusion about Fedora RPM patches follows from
that comparison.

`tag_ref: false` skips Pungi's direct ref write in
[tree.py lines 93--98](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/ostree/tree.py#L93-L98):
`rpm-ostree compose tree --write-commitid-to` and runroot use the same mounted
repository. The notification after the runroot work
([ostree.py lines 150--158](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/phases/ostree.py#L150-L158))
hands off to the signing handler, which is expected to let robosignatory create
the signature and update the ref. The [signing handler's checks](https://forge.fedoraproject.org/pungi/pungi/src/commit/2265c1375c81cfd9ca2d60743479ea0d9592d2c4/pungi/scripts/wait_for_signed_ostree_handler.py#L105-L109)
only tests `.commitmeta` existence and raw ref equality, not whether the
`.commit` object loads.

Instrument the selected ref's old target and old commit loadability *before*
compose. After successful compose, record the new commit ID and stamp and test
new commit loadability. On failure, make this best effort and preserve the
original exception. On the host, also record bounded before/after events around
the notifier handoff (the notifier phase is at `ostree.py` lines 150--158).
In the signing handler, record start, first `.commitmeta`, and first
`ref == new-commit` transitions, and test the new `.commit` loadability at the
latter two points. Do not log every polling iteration. Robosignatory should
independently record before/after writes for actual attribution.

Failure-only probes cannot observe the original ref/object divergence if
compose stops before the relevant transition. A commit can be unreferenced
during the signing handoff, so concurrent pruning is a hypothesis, not an
established mechanism: no prune code was found in core Pungi. Keep probes
small--no full `fsck`, mutation, or global-ref snapshots--and correlate task,
ref, arch, repository, and timestamps; make the task directory and its logs
accessible. Proposed tests (not run) cover mocked success, failure,
`tag_ref: false`, probe failure preserving the original exception, and a
signature/ref update without a loadable commit.

## Cause status and remaining unknowns

The evidence proves only that the six refs observed on September 9 named
unavailable *published* commit metadata and that the captured compose attempts
could not load their previous SELinux policy. It does not establish whether
origin storage has the objects, whether publication/synchronization or pruning
caused their absence, when the refs first became bad, or the health of the six
currently reachable commits.

No specific recent `ostree` or `rpm-ostree` change has been identified as the
cause. Recent tags mentioned in [the ticket](https://forge.fedoraproject.org/releng/tickets/issues/13509#issuecomment-1350621)
are temporal correlation only; this was not a comprehensive upstream code
audit and therefore neither establishes nor excludes a software cause.

Unconfirmed hypotheses are incomplete publication or synchronization ordering
(a ref becoming visible before its object), object pruning/removal, or a ref
rollback to an already removed object. Multiple variants and architectures make
shared compose or publishing infrastructure a plausible common factor, but the
multi-ref evidence does not distinguish that from a common software defect.
Compare origin and published availability, retain publication/sync/prune logs,
and perform OSTree verification in a repository before attributing cause or
declaring any reachable ref healthy. IoT Rawhide composes were reported as
working in [the incident discussion](https://forge.fedoraproject.org/releng/tickets/issues/13509#issuecomment-1352748),
but were not independently verified here.
