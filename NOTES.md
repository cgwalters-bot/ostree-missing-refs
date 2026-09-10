# Incident notes: Fedora releng #13509

_Research updated 2026-09-09._

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

Thus 6/12 published refs point at unavailable commit metadata objects: both
architectures of COSMIC-Atomic, Kinoite, and Silverblue. The active compose
configuration includes exactly those six variant/architecture pairs, as well
as Onyx and Sericea; the latter pair's configured arches are not a claim that
they were built in either captured compose.

## Captured compose evidence

The collector retained complete captures:

- `/var/tmp/ostree-missing-refs-logs-J2w5Ac`: `Fedora-Rawhide-20260826.n.0`,
  13 files / 812,012 bytes.
- `/var/tmp/ostree-missing-refs-logs-fbfAxV`: `Fedora-Rawhide-20260909.n.0`,
  15 files / 821,909 bytes.

Reproduce them with `ostree-missing-refs logs --compose
Fedora-Rawhide-20260826.n.0` and `ostree-missing-refs logs --compose
Fedora-Rawhide-20260909.n.0` (or `ostree-missing-refs logs` to pin current
`latest-Fedora-Rawhide/COMPOSE_ID`). Their source base is
[`compose/rawhide/`](https://kojipkgs.fedoraproject.org/compose/rawhide/).

Both captures record the same six `ostree` failures and the same previous
checksums listed as absent above. Every collected `create-ostree-repo.log`
ends with `Loading previous sepolicy: No such metadata object <checksum>.commit`;
for example, the [latest COSMIC aarch64 log](https://kojipkgs.fedoraproject.org/compose/rawhide/Fedora-Rawhide-20260909.n.0/logs/aarch64/COSMIC-Atomic/ostree-2/create-ostree-repo.log)
and [August Kinoite x86_64 log](https://kojipkgs.fedoraproject.org/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/Kinoite/ostree-3/create-ostree-repo.log).
The compose configuration is materially unchanged in the relevant sections:
all five OSTree variants use `atomic-desktops/config.git` branch `main`, the
shared `/mnt/koji/compose/ostree/repo/`, `force_new_commit: true`,
`unified_core: true`, and `selinux-policy-targeted`. The emitted
`rpm-ostree` version is `2026.2` in the collected create logs; Pungi is
`pungi-4.13.0-1.fc44.noarch` in both global logs. This is correlation, not a
cause determination.

Several other compose failures are separate evidence: the latest capture has
an `ostree-container` COSMIC aarch64 failure whose global log reports a
temporary config-Git copy error, and both captures report image-builder and
KIWI failures (including MiracleWM; August also SoaS). They do not establish
missing OSTree objects. The collected `runroot.log` files also summarize
`BuildrootError: rm exited with status 1`; the adjacent create logs identify
the concrete OSTree failure above, so the summary alone should not be treated
as a distinct root cause.

## Cause status and remaining unknowns

The evidence proves only that these six published refs name unavailable
published commit metadata and that compose attempts cannot load their previous
SELinux policy. It does not establish whether origin storage has the objects,
whether publication/synchronization or pruning caused their absence, when the
refs first became bad, or the health of the six currently reachable commits.

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
