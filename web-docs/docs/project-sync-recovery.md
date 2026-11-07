# GitHub Projects synchronization recovery (#249)

The automatic and manual Projects sync workflows are **disabled pending operator setup**.
Do not enable either workflow merely because this code is merged.

## Changes and scope

- Automatic sync now accepts issue and pull-request events without comparing their
  event ref to a branch name. Scheduled reconciliation is restricted to the
  repository's default branch ref.
- Both workflows require `FLOWERLY_PROJECT_TOKEN` explicitly and fail visibly
  if it is absent. They must not fall back to the read-only `GITHUB_TOKEN`.
- `GITHUB_TOKEN` remains read-only; project writes require a separately
  managed, least-privilege credential.

## Operator-only acceptance steps

1. Create a token authorized to write the Doldas-owned Project 7, scoped as
   narrowly as the GitHub Projects API permits. Store it in the repository
   secret `FLOWERLY_PROJECT_TOKEN`. Do not commit, print, or paste its value.
2. Have the project owner verify who can access and rotate that credential.
3. Manually enable `project-sync-manual.yml`, dispatch once, and verify
   that existing issue/PR cards and their status, sprint, priority and release
   fields match the tracker without duplicate cards.
4. Verify the scheduled and issue/PR trigger behavior using actual workflow
   runs. Only then re-enable `project-sync.yml`; inspect any failure before
   concluding the board has converged.
5. Record run URLs, board audit results and any remaining discrepancies on
   issues #249 and #257. Do not mark them complete based on static tests.

## Offline verification

Run `python3 scripts/test-project-sync-workflows.py`, followed by
`bash scripts/preflight-docs.sh`. These check the source-level guard;
they do **not** test GitHub secret availability or Projects v2 permissions.

Status (2026-11-07): code fix proposed; credential, workflow enablement,
live dispatch, and board verification **not performed**.
