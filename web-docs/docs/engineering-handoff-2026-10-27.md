# Engineering handoff — 2026-10-27

## Source of truth and authority

GitHub issues, the checked-out code, actions and actual branch SHAs take
precedence over dated notes. Treat earlier issue comments and release-plan
tables as historical. Use the current HEAD before attempting another PR.
The maintainer requires explicit approval for `release/2` → `master`,
for `v0.0.4` tagging and release publication; no CI override.

## Confirmed shipped state

- `master` `2d719551ff5a14a9231f51a323d3a7c6f57bbd88` was
  squash-promoted from candidate `6b7f363` via #206.
- `release/2` advanced after #250 (ancestry merge), #251
  (systemd harness), #253 (PTY IPC test framing), and #254
  (promotion/check reporting); observed `c19f9e1`.
- Draft promotion PR #255 is **not** authorization to merge.
- #72/#74/#184 closed after accepted exact-SHA Arch evidence. Do not
  reopen merely because historical unchecked lists remain in issue bodies.
- #187 remains open for individual Scorecard findings and maintainer
  repository-setting decisions. #249 remains open for disabled
  Projects-sync automation and missing scoped token.
- #31 and #8 remain open until final release acceptance and explicit
  publication approval; #50 is the PTY design parent, with ADR-0016
  accepted and descendant checks #72/#73/#74 proven.

## Incident timeline and distinction

| Symptom | Scope | Evidence | Disposition |
| --- | --- | --- | --- |
| `rustc` SIGSEGV | hosted runner release build | push 40536428363 attempt 1, Rust 1.99.1 LLVM | Intermittent; no proven root cause; retain diagnostics |
| Truncated `session.read` JSON | guest PTY flood test | attempt 2, short framed response in Python client | #253 exact-read and negative regression tests; push 40572253896 Arch PASS |
| `start-limit-hit` | six fast systemd test restarts | #251 isolated reproduction | Reset failed *only in harness*; shipped unit unchanged |
| Early promotion validation failure | master PR vs pending release push | check demanded completed exact-SHA evidence | #254 wait and fail-closed handling |
| Required policy status absent | `if` excluded `fix/*`; master excluded | GitHub effective required-check ruleset | #254 always report check, validate source branches |

## Milestone and project-board reconciliation

GitHub Projects v2 owner board: `https://github.com/users/Doldas/projects/7`.
The installed GitHub connector does not expose an authorized Projects or
milestone-list/update endpoint (beyond assigning an existing milestone to
individual issues). Therefore **board field values and milestone dates have
not been asserted as updated**. Work remaining for a credentialed maintainer:

1. Inventory all milestones with open/closed counts, due dates and scope.
2. Ensure the 0.0.4 release tracker #8, architecture #31/#50, security
   #187, automation #112, and Projects #249 have correct milestone
   allocation; keep future #3/#4 on their intended 0.0.6/0.0.8 milestones.
3. Synchronize the board's Sprint, Release, Status and review fields
   from the actual issues and merged PRs, preserving unresolved alerts.
4. Fix #249's event filter and add a scoped project-write credential before
   re-enabling sync; do not grant default GITHUB_TOKEN write permissions.
5. Do not close a milestone solely because code was promoted to master.
   Tag and release-publication approval are distinct.

## Next agent checkout preflight

1. Read `AGENTS.md`, this handoff, and
   `web-docs/docs/release-0.0.4-completion-plan.md`.
2. Check GitHub PRs #254/#255, issues #8/#31/#50/#112/#187/#249 and the
   full SHA of `release/2`; confirm CI for the **current** SHA.
3. Inspect exact-SHA push Arch logs, including stdout from
   `PTY_CONTAINMENT_ADVERSARIAL`, before any further promotion.
4. Keep deployment PTY fail-closed, production start-rate limits, owner
   isolation and security workflow least privilege unchanged.
