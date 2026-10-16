# #72 broker phase 2 — protocol and recovery guards

**Status: Implemented source, exact-SHA CI and operator acceptance pending. Production PTY remains blocked.**

This follow-up hardens the standalone, non-root broker foundation introduced in PR #120:

- Reads exactly one newline-terminated JSON request, with an upper bound of 4096 bytes; incomplete, oversized and trailing-command payloads fail closed. Fragmented valid messages can be received without assuming a single socket read.
- Caps reservations to 64 in-memory-owned Session groups.
- Refuses startup if the delegated subtree already contains any recognized PTY Session cgroup. The broker **does not** silently adopt possible survivors after a crash; independent privileged cgroup recovery is still required.
- Adds protocol regression tests for fragmented requests, concatenated commands, truncation, oversized frames and reservation exhaustion.

## Important limitations and next phase

This does **not** prove safe cross-UID process placement, durable ownership, successful guardian/broker crash cleanup or the Arch production containment test. In particular, the broker still does not wire into guardian; it should not be enabled for shell sessions. Refusing unknown surviving Session cgroups intentionally trades availability for safety. The trusted systemd socket topology, authenticated stateful ownership and kernel-pinned process placement are still unimplemented.

Do not change the adversarial Arch escape assertion, the PTY-root prohibition, the accepted single SQLite state.db, the security CI gates or #55. Require source-format checks, the new Python tests, CodeQL/security and unchanged Arch guest acceptance on the eventual *integrated* design. #72 remains open.
