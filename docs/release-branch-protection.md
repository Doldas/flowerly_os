# Required release branch checks (P0 #55)

This is an **administrative release gate**. Successful GitHub Actions runs are
not equivalent to required checks. The managed ruleset in
`.github/release-gates.json` is a desired configuration, **not** proof that
GitHub is enforcing it. Do not close #55 until the remote checks and a negative
merge-blocking probe pass.

## Audited state before implementation

On 2026-10-13, `release/2` at
`d961227c590b06c104c229453e036cd7a896b3d6` reported `protected=false`
and no required checks. The active `MERGERULES` ruleset (24880732) applied
only to `~DEFAULT_BRANCH`, had an empty status-check array, and granted user
4792047 an `always` bypass. The connected GitHub integration can read
rulesets but **cannot create or modify** repository rulesets.

## Managed gate

Install the separate `FLOWERLY_RELEASE_REQUIRED_CHECKS` branch ruleset for
both the default branch and `refs/heads/release/*`. It layers with
`MERGERULES` without overwriting or relaxing its existing review rules.

- Require a pull request, resolved review threads, and **no bypass actors**
  in the managed ruleset. Release branches do not require a separate approver;
  master still inherits its existing review requirements from `MERGERULES`.
- Require the latest merge base to pass each named check; GitHub Actions
  integration ID **15368** was verified from live check-run metadata.
- Fail closed if any required job is missing or skipped. Security and CodeQL
  now trigger on documentation-only PRs too.
- Required contexts: `rust`, `docs`,
  `Validate branch policy and issue link`, `cargo-audit`,
  `cargo-deny (advisories)`, `cargo-deny (bans licenses sources)`,
  and `CodeQL (rust)`. These are **job/check-run names**, not workflow names.
- `cargo audit` and `cargo deny advisories` must return success.
  The previous `continue-on-error` on advisories was removed. Existing
  `deny.toml` warning-level bans (such as wildcards) still produce warnings;
  this policy does not turn all warnings into errors.
- Workflow runs on untrusted PRs remain on disposable hosted runners.
  The persistent `flowerly-runner` is not used for PR CI.

GitHub administrators may still **change or disable** rulesets. No policy
written in the repository is immune to repository administrators. The
existing default-branch `MERGERULES` has an owner bypass, but the new
managed ruleset has no bypass actors; verify the effective *combined* rules,
not just one ruleset.

## Installation and verification (administrator)

First review `.github/release-gates.json` and run the offline regression
suite (no credentials or network access):

```sh
python3 scripts/release-gates.py validate
python3 scripts/test-release-gates.py
```

Use an owner-approved token with repository administration permission,
available **only in the maintainer's local environment**, never in a workflow
or committed file. The connector used for the implementation cannot perform
this privileged installation.

```sh
# Use an existing authenticated GitHub CLI session; do not paste tokens into history.
GH_TOKEN="$(gh auth token)" python3 scripts/release-gates.py apply --confirm-apply
GH_TOKEN="$(gh auth token)" python3 scripts/release-gates.py verify
```

The script only creates/updates its dedicated named ruleset; it does not
alter `MERGERULES`. It verifies GitHub's effective branch rules for
`master` and `release/2`. If verification fails, do not claim #55 is done.
Use the [repository rulesets page](https://github.com/Doldas/flowerly_os/settings/rules)
to inspect enforcement, including bypass actors and merge queue behavior.

## Negative merge-blocking acceptance probe

After installation, open a **draft** `feature/0.0.4-*` PR targeting
`release/2` (do not merge it) with a deliberately failing Rust CI test or
required security check in that PR's branch. Verify that GitHub's PR interface
shows the required check as failing, and that the Merge control is blocked
for a user who has no bypass permission. Record the PR URL, required check
name, failing head SHA, mergeability/ruleset evidence and timestamp in #55
and #31. For a non-draft probe PR with a failing required check, run:

```sh
GH_TOKEN="$(gh auth token)" python3 scripts/release-gates.py verify --probe-pr NUMBER
```

This read-only check requires both a failed required check on the current PR
head and GitHub `mergeable_state=blocked`. Repeat with a docs-only PR to
show that the security and CodeQL checks run rather than going missing.

Do not conflate a red test with blocked merge enforcement: both the failing
check **and the blocked merge state** must be observed. Do not merge the probe.
Also verify direct-push prevention using a disposable branch/ruleset test,
not by pushing to `release/2` or `master`. GitHub administrators can always
reconfigure protections, so document that trust boundary explicitly.

## Limits

These rules secure repository integration, not process persistence. PTY
ownership, Flow A → B → A Linux acceptance, and the full Arch VM test remain
independent 0.0.4 gates (#50, #31). Do not close #55 before actual GitHub
ruleset installation and negative probe evidence.
