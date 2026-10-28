# 0.0.4 completion plan and session handoff

Status: **v0.0.4 published 2026-10-28.**
Annotated tag `v0.0.4` points at `master`
`b13ce02b0eb1fd88471b17b8dc60a7ea962c630c`. The tag release workflow
[40753771279](https://github.com/Doldas/flowerly_os/actions/runs/40753771279)
passed the final fully upgraded Arch VM gate, `just verify`, the release build
and publication: [release notes](https://github.com/Doldas/flowerly_os/releases/tag/v0.0.4).
Remaining open items are administrative/security: #187 (Scorecard alerts and the
owner ruleset bypass), #112 (release forwarding), and #249/#257 (Projects
metadata). Known limitations: the shipped unit has no `Delegate=` (PTY spawning
disabled by default) and the Cargo workspace version still reports `0.0.1`.

Authoritative trackers: roadmap [#8](https://github.com/Doldas/flowerly_os/issues/8),
hardening gates [#31](https://github.com/Doldas/flowerly_os/issues/31).
Issues and code win over this page; re-verify branch SHA, PR state and CI
before acting (see `AGENTS.md`).

## How work lands

1. Branch from current `release/2` (`fix/<issue>-…`, `feature/…`, `bugfix/…`, `docs/…`).
2. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace` and `bash scripts/preflight-docs.sh` **from a clean
   checkout** (a leftover `target/flowerly-arch-rootfs/` makes the rootfs fixture
   refuse to run; use a scratch `git worktree`).
3. Open a PR into `release/2`, link the issue, wait for exact-head `rust`,
   `docs`, CodeQL and security checks. The owner has authorized squash-merging
   green, scoped implementation PRs into `release/2`.
4. Merged changes accumulate in the standing **`release/2` → `master` promotion
   PR** (currently #255). The owner merges that PR manually when 0.0.4 is ready;
   `release/2` is kept afterwards for continued development. Never auto-merge
   master.
5. Comment on the affected issue with merged SHA, evidence and what remains,
   and update the workstream table below.

## Workstreams

| # | Workstream | Issue(s) | State | Next concrete step |
| --- | --- | --- | --- | --- |
| 1 | Typed storage error provenance | #62, #56 | **Done**: classifier (#207), stage-aware deadline errors and per-request timing (#211); #62 closed. | — |
| 2 | Security alert backlog | #187 | Per-alert evidence, impact and disposition in [triage](security-alert-triage.md); no dismissals. #243: least-privilege workflow tokens and the CI policy check `test-workflow-permissions.py` (alerts 92 and 93 close after promotion). | **Maintainer:** branch-ruleset bypass actors and deletion/non-fast-forward rules for `release/*` (alert 1); keep alert 2 open until `SECURITY.md` is on `master`. |
| 3 | Dynamic counts and readiness | #31 Gate B | **Done**: merged #209 (one-snapshot counts; `ready` only when the DB answers). | — |
| 4 | Verified stale-socket recovery | #31 Gate B | **Done**: merged #210 (instance `flock` plus verified stale removal; PTY SIGKILL test requires it). | — |
| 5 | Machine-readable errors everywhere | #31, #53 | **Done**: #53 closed (#205, #213, #216–#218, #223); #31 item ticked with evidence on 2026-10-27. | — |
| 6 | CLI priority contract | #31 Gate A | **Done**: `check-cli-contracts.sh` covers omitted, explicit and invalid priority (re-verified 2026-10-24). | — |
| 7 | Administrative authorization | #31 | **Done**: #222, #227, #234, #235; `test-admin-authorization.py --cross-uid` passes in the Arch guest; #31 item ticked with evidence. | — |
| 8 | PTY descendant containment | #72, #50 | **Done for 0.0.4 acceptance:** #246 adversarial namespace/PID/exhaustion/orphans suite verified in exact-SHA Arch VM push 40507915398 and dispatch 40507977240 on `6b7f363`; #72 closed, architecture #50 closed 2026-10-27. | Shipped systemd unit still deliberately disables PTY without delegation; future enhancements tracked separately. |
| 9 | PTY streaming / attachment | #73 | **Done**: long-poll transport and races (#215), writer-lease expiry (#219), PTY limited to the service account (#222). Owner accepted ADR-0016 on 2026-10-25; #73 closed. | — |
| 10 | PTY crash/restart truth | #74, #63 | **Done:** #244 crash matrix and cgroup leak fixes passed exact-SHA Arch release candidate `6b7f363`; #74 closed with evidence. | Maintain failure-injection coverage; no outstanding 0.0.4 implementation blocker. |
| 11 | Arch release VM stability | #184 | **Acceptance done, incident follow-up ongoing:** #245 diagnostics, runner sizing and exact-SHA push + dispatch passed `6b7f363`; #184 closed. Separate Rust SIGSEGV occurred in later CI on `826bfb1`; retain diagnostics. | Re-test every new release head; do not assume prior commit's evidence transfers. |
| 12 | Release evidence | #8, #31, #55 | **Code promoted, publication pending:** 0.0.4 candidate exact-SHA Arch accepted and #206 squash-promoted to `master` `2d71955`. #247 migration/rollback evidence completed. Subsequent changes #250/#251/#253/#254 land on `release/2`; #255 is draft. | Owner release-tag/publication approval, current SHA acceptance and #187/#249 administrative follow-up. |
| 13 | Per-user data ownership | #225 (P0) | **Done**: PRs 1–8 (#226 through #236) enforce ownership for every domain and add audited Admin promotion and ownership commands; #225 closed 2026-10-26 with Arch guest evidence. The shipped default is service-account-only; multi-user admission is the opt-in drop-in `/usr/share/flowerly/systemd/flowerlyd-multi-user.conf`. See [command authorization](command-authorization.md). | — |
| 14 | Database path override | #237 | **Done**: #238 unsets `AGENTIC_DATABASE_PATH` in the unit and the drop-in, and logs a warning when the path leaves the state directory. | — |
| 15 | PTY isolation override | #240 | **Done**: #241 unsets `FLOWERLY_PTY_ISOLATION` and `FLOWERLY_PTY_CGROUP_ROOT` in the unit and the drop-in; `flowerlyd` warns in development isolation. | — |
| 16 | Dependency updates | #231 | **Done**: tokio 1.54.1 ported in #242 (lockfile v3 kept); #231 closed without merge (it targeted `master`). | — |

Items 8–12 were release-accepted on exact SHA `6b7f363` in the actual Arch VM.\nEvery later candidate needs its own exact-head push CI and Arch evidence; do\nnot reuse older runs for a changed tree or skip owner promotion approval.

## Non-negotiables

- Never weaken root-shell refusal, delegated-cgroup requirements, PTY
  cleanup, Arch guest acceptance or exact-commit promotion gates to get green.
- Never skip, disable or quarantine a test.
- Do not add RFC 0004 / #64 future work to 0.0.4 scope.


## Release integration and engineering log — 2026-10-27

This section supersedes old `next concrete step` entries for #72/#74/#184
and any reference to PR #206 as open.

- **Accepted 0.0.4 candidate:** `6b7f363`; exact-SHA push run
  [40507915398](https://github.com/Doldas/flowerly_os/actions/runs/40507915398)
  and repeat dispatch [40507977240](https://github.com/Doldas/flowerly_os/actions/runs/40507977240)
  verified Arch cgroups, namespace/PID defenses, PTY crash/restart, systemd
  admission and failure classification. #72, #74 and #184 closed.
- **Promotion:** PR #206 merged as squash commit `2d71955` on `master`.
  Release publication/tagging has **not** been authorized.
- **History reconciliation:** #250 merged using a two-parent commit into
  `release/2` (`3fb206d`), preserving `master` ancestry; #251
  fixed an intermittent six-start systemd harness failure (`826bfb1`).
- **Follow-up Arch test discovery:** push CI attempt 1 on `826bfb1`
  hit `rustc` SIGSEGV; attempt 2 completed compilation but exposed
  truncated JSON in the PTY flood harness. #253 added complete length-framed
  socket reads and fragment/truncation regression tests. Exact-SHA push CI
  [40572253896](https://github.com/Doldas/flowerly_os/actions/runs/40572253896)
  on `b8c86f6` passed including executed Arch guest.
- **CI orchestration:** #254 merged on `release/2`. Promotion validation now
  waits for exact-SHA successful push CI/security/CodeQL rather than racing
  unfinished runs; release PR policy runs unconditionally, supports `fix/*`,
  and master promotion PRs emit the required policy status. The latest
  `release/2` and draft #255 must still be independently verified.
- **Security:** #187 remains open; documented Scorecard findings are tracked
  individually in [security-alert-triage](security-alert-triage.md). Passing
  CodeQL/security workflows is not a claim that all scanning alerts are closed.
- **Projects:** #249 remains open: Projects sync workflows were disabled
  because of a false job condition and missing project-write token. The
  GitHub Project board must be reconciled by an authorized project writer;
  never claim a synchronized board based solely on issues.
- **PTY shipped behavior:** the standard systemd unit has no `Delegate=`;
  persistent PTY spawning is disabled by default. Multi-user access is
  opt-in. Arch delegated tests do not change the shipped defaults.

### Post-publication follow-up

1. **Done:** `v0.0.4` was tagged and published from
   `b13ce02b0eb1fd88471b17b8dc60a7ea962c630c`; exact-tree CI, security, CodeQL,
   nightly Arch and the tag Arch gate all passed.
2. **Open:** #187 — resolve or explicitly accept the remaining OpenSSF Scorecard
   findings and decide on the owner ruleset bypass. Nothing was dismissed.
3. **Open:** #112 release forwarding and #249/#257 Projects board/milestone
   automation.
4. **Next release:** bump the Cargo workspace version and provide a reviewed
   delegated PTY systemd configuration before claiming default PTY spawning.

## Publication log — 2026-10-28

- Created annotated tag `v0.0.4` on the approved `master` commit
  `b13ce02b0eb1fd88471b17b8dc60a7ea962c630c`; no tag existed before.
- Release workflow [40753771279](https://github.com/Doldas/flowerly_os/actions/runs/40753771279):
  final Arch kernel validation passed in 37m05s, `just verify` and the release
  build passed, and the GitHub Release was published.
- Published assets `flowerlyd`, `evolutiond`, `providerd`, `agenticctl` and
  `SHA256SUMS`; checksums were independently re-downloaded and verified.
- Release notes were replaced with the accurate v0.0.4 section, exact SHA,
  evidence links and known limitations.
