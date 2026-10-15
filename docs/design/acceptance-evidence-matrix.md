# Evidence ledger and cross-mode acceptance gates

**Status:** Proposed verification plan for #50 #72-#74 #79 #82-#95. **No new test is claimed to have run.**

## Evidence rules

Each claim must include exact Git SHA, workflow run/job URL, architecture, Linux kernel/Arch guest version, effective UID, cgroup layout and delegation, relevant storage schema, test arguments, relevant failure log and resulting artifacts. Classify **Proposed** (design only), **Implemented** (source exists), **Tested** (that exact build passed a named scenario), **Accepted** (owner-approved criteria and evidence). GitHub workflow success on a PR is not equivalent to release-head CI success. Skips, invalid test preconditions and guest package setup failures are **Not tested**, not pass.

## Release blockers / existing evidence

* Previous release run on `5e55dfa75096004e1af46546415a4b779434114c`: `38646432475` failed at Arch guest `pacman` space check (`152242 blocks needed, 53741 free`), before positive cgroup acceptance. Earlier failure `38643678286` had the same class. See #79.
* #50/#72: verify malicious same-UID sibling cgroup migration, setsid/grandchildren, cgroup.kill and resource caps in real delegated guest; a nondelegated fail-closed pass is insufficient.
* #73: one writer, multiple readers, slow/disconnected clients, cursor overflow, attach/write/terminate races and replay without blocking SQLite.
* #74: guardian SIGKILL, sessiond SIGKILL, ambiguous spawn, restart, stale socket and honest state without orphaned live descendants.

## Contract matrix

| Area | Mock/unit | Real SQLite/Linux | Real Arch/KVM / cluster | Gate |
| --- | --- | --- | --- | --- |
| Typed repositories #83 | actor, operation validation and error semantics | real SQLite transactions, FK and triggers | service lifecycle under real restart | Before #83 accepted |
| Physical migration #84 | parser/state machine | backup, WAL, injected EIO/ENOSPC/crash, manifest/rollback | real guest power interruption and recovery | Before any cutover |
| Modes #85/#88 | illegal mode/backend combinations; fail-closed | missing/corrupt memory DB | offline CoreOnly and Standalone boot, no new listeners | Before mode shipping |
| Embeddings #86 | dimension/NaN/tenant/delete/ranking | local index rebuild & retention | offline performance and isolation | Before memory shipping |
| Cluster #87 | policy/peer mocks | actual libSQL behavior | 3-node partitions, malicious peers, revocation, merge | Before Cluster exposure |
| Host facade #92/#95 | owner caps / fault mapping | Linux peer and cgroup readback | adversarial real delegated cgroup and KVM | Before PTY host extraction |
| Dependency gates #93 | failing forbidden-import fixture | Cargo feature compilation | review allowlist security regressions | Before imports enforced |
| Contract parity #94 | fake-failure matrix | real adapter same semantics | kernel negative and positive cases | Before facade acceptance |
| Operations #89 | bounded memory/resources | backup, contention, disk full, corruption | full recovery and exact-SHA reports | Before release readiness |

## Mandatory fail-closed negative scenarios

Untrusted client-supplied actor or PID must not change host authority. No root-owned interactive PTY shell. No fallback from malformed config to network Cluster. No cross-DB phantom atomicity, dropped receipt, provenance split, privileged mount on caller-provided path, false success on no-op, fabricated telemetry, implicit AI-provider requirement for CoreOnly, or unsafe peer enrollment from mDNS alone.

## CI repair policy (#79)

Diagnose guest image size, package cache and upgrade footprint as **test environment** issues. Fix image provisioning, disk allocation or reproducible package acquisition in an isolated PR only after verifying expected minimum space; preserve all release security checks and do not skip the positive matrix. Re-run on new exact SHA and retain console/report artifacts; this document does not close #79.
