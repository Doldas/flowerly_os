# Security alert release review (0.0.4)

Related: [security acceptance #187](https://github.com/Doldas/flowerly_os/issues/187).

A successful CodeQL, cargo-audit or cargo-deny workflow does not prove
the GitHub Security alert backlog is empty. Neither this script nor an
empty alert inventory certifies the release.

## Maintainer workflow

On a trusted workstation, authenticate `gh` with permission to read
Code Scanning, Dependabot and Secret Scanning alerts.

```bash
umask 077
python3 scripts/security-alert-inventory.py Doldas/flowerly_os > security-alerts.private.json
python3 scripts/security-alert-triage.py security-alerts.private.json
```

The inventory is redacted but contains private security metadata. Do not
commit it, publish it as a CI artifact or paste raw secret values in issues.
Prefer storing it outside the repository; dispose of it following incident policy.

Triage emits only aggregate totals and priority buckets:

- Exit `0`: no open alerts **in this snapshot**; other release gates and
  freshness/authorization review remain required.
- Exit `1`: open alerts or API retrieval failure; release review blocked.
- Exit `2`: missing, incomplete, malformed or inconsistent inventory;
  release review blocked.

Any open secret-scanning alert requires immediate review without exposing
its contents, type or validity. Unknown severity is unresolved, not safe.
Missing API access is not evidence of zero alerts.

## Release acceptance evidence

1. Record exact release SHA, inventory timestamp, reviewer and authorized
   access to all three alert APIs.
2. Investigate alerts in GitHub Security, prioritize critical/high findings,
   and record ownership, exploitability, remediation PRs and negative tests.
3. For dismissals, record evidence, approver and compensating controls.
4. Verify exact-SHA CodeQL, cargo-audit, cargo-deny, PTY/Arch kernel
   acceptance and protected promotion before explicit maintainer sign-off.
5. Link findings and resolutions to #187, #31 and #8 without secrets.

The scripts neither dismiss alerts nor bypass release protection.

## Triage snapshot 2026-10-27 (`release/2`)

Open alerts: **9 Code Scanning (all OpenSSF Scorecard), 0 Dependabot,
0 Secret Scanning.** There are no open CodeQL, cargo-audit or cargo-deny
findings. Scorecard analyses **`master`**, so a fix on `release/2` closes an
alert only after promotion and the next Scorecard run.

Policy (owner, 2026-10-27): each alert is judged on its own evidence. Being
reported by Scorecard is never a reason to call it harmless. Alerts are fixed
by focused hardening, or stay open with a documented rationale. **None is
dismissed** without a concrete, independently reviewable justification and
the maintainer's approval recorded here and in #187.

| Alert | Rule | Scorecard evidence | Impact | Disposition |
| --- | --- | --- | --- | --- |
| 93, 92 | TokenPermissions | `project-sync.yml` and `project-sync-manual.yml`: "no topLevel permission defined". | **Real.** Without a block, jobs get the repository default token. | **Fixed on `release/2`**: top-level `contents: read`, `issues: read`, `pull-requests: read`. Project writes need `FLOWERLY_PROJECT_TOKEN`. `scripts/test-workflow-permissions.py` (CI `rust` job, with regression tests) now requires a write-free top-level block in every workflow and allows write scopes only on recorded jobs. `docs-pages.yml` also moved `pages`/`id-token: write` from top level to the two jobs that publish. Closes after promotion. |
| 113 | TokenPermissions | `release.yml:75`: "jobLevel 'contents' permission set to 'write'". | Needed: the `release` job publishes the GitHub release, which requires `contents: write`. It runs only for `v*` tag pushes, after the exact-tag Arch VM job, and is the only job with that scope (enforced by `ALLOWED_WRITES`). | **Stays open**, accepted as required. A dismissal ("won't fix") needs maintainer approval. |
| 1 | BranchProtection | Score 6: "settings apply to administrators" disabled; required approving review count 1; "no status checks found". | **Partly real.** Rulesets `MERGERULES` (24880732, default branch: deletion, non-fast-forward, linear history, PR with 1 approval) and `FLOWERLY_RELEASE_REQUIRED_CHECKS` (25024564, default branch and `release/*`: required `rust`, `docs`, branch policy, cargo-audit, cargo-deny, `CodeQL (rust)`) are active. Scorecard does not read rulesets for status checks. Two real gaps: (a) both rulesets list the repository owner (`User`) as a bypass actor, which is the "administrators" warning; (b) `release/*` has required checks but **no deletion or non-fast-forward rule**. | **Stays open. Maintainer decision** (repository settings are not changed by agents): consider adding deletion and non-fast-forward rules for `release/*`, and decide whether the owner bypass is still needed. |
| 33 | CodeReview | "Found 0/8 approved changesets". | **Real** process risk: changes are reviewed by the owner and CI, not by a second human. | **Stays open.** Accepted for the solo-maintainer stage; it is mitigated by required checks, exact-SHA Arch evidence and manual master promotion. Not dismissible as a false positive. |
| 34 | Maintained | "project was created within the last 90 days". | Informational. It reflects repository age, not code. | **Stays open.** It re-evaluates automatically once the repository is 90 days old. |
| 32 | Fuzzing | "no fuzzer integrations found". | **Real gap.** The IPC frame decoder and SQLite row parsing handle untrusted input from local peers. | **Stays open.** Tracked as future hardening, not a 0.0.4 gate. A focused issue for `cargo fuzz` targets on the IPC decoder is recommended. |
| 35 | CIIBestPractices | "no effort to earn an OpenSSF best practices badge". | Informational. | **Stays open.** Optional; a maintainer decision. |
| 2 | SecurityPolicy | Score 4: "security policy file detected; no linked content found". | Minor. `SECURITY.md` on `release/2` links the private advisory URL, but `master` does not have it yet. | **Stays open** until `SECURITY.md` reaches `master` and Scorecard re-evaluates (owner decision 2026-10-27). |

The 2026-10-24 snapshot recorded the same nine alerts without the evidence
column; the assessments above supersede it.
