# Layer 1.5 privileged process and cgroup threat review (#95)

Status 2026-11-07: **partial source-level prevention and threat inventory**. No kernel authority has moved and no privileged attack testing has been claimed.

## Assets and trust boundaries

- Trusted: kernel-attested IPC peer identity, current service-account PTY supervisor/guardian, delegated cgroup v2 root, process lifecycle and release Arch acceptance. These are not replaceable with mock success.
- Untrusted: caller-supplied Flow, Session, PID, path, mount point, model output, process observation and stale guardian reference.
- The high-level `flow`, `goal`, `generation`, `provenance`, `agentic`, `ownership`, `capability` and `authorization` modules must not directly import process spawning interfaces or embed cgroup control paths. The source guard checks only these modules, not trusted PTY/IPC or host adapters.

## Threat and required real acceptance

| Threat | Required live outcome and evidence |
| --- | --- |
| Arbitrary PID or PID reuse | Owner/session bound process identity; reject arbitrary PID on real kernel |
| Same-UID sibling cgroup migration | Enforce delegation and deny migration escape in delegated Arch guest |
| `setsid` descendant escape | Descendants stay contained and are reaped |
| Guardian or daemon SIGKILL | No orphan processes, correct restart and uncertainty semantics |
| Symlink/cgroup/mount traversal | No arbitrary mount or cgroup write from caller path |
| Wrong UID or stolen capability | Typed denial and no cross-owner PTY control |
| Missing delegation / permission denied | PTY disabled, no successful no-op or fake telemetry |
| Disconnect during spawn | Bounded cleanup and no duplicated committed effects |

## Limits and owner review

`scripts/test-host-process-authority.py` is a narrow CI source guard. It cannot detect indirect aliases, macros, dynamic paths, existing trusted components or actual privileged behavior. Real Arch acceptance, #50/#72/#74 evidence, #91/#92 design approval and security owner sign-off are still required. No completed #95 acceptance is claimed.
