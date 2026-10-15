# Bootstrap the native Flowerly OS GitHub Project

The source of truth remains the [release roadmap](../roadmap.md), [Project overview](index.md), and [review-driven implementation plan](../review-implementation-plan-2026-10-12.md). These files are on the `release/2` branch as of October 12, 2026.

The helper at [`scripts/bootstrap-github-project.py`](../../scripts/bootstrap-github-project.py) creates or refreshes a **real GitHub Projects v2 project owned by Doldas and linked to this repository**. It does not create a substitute Markdown board, change the state of an issue, or duplicate existing issue records.

## Run

Requirements: Python 3.9+, [GitHub CLI](https://cli.github.com/), and a GitHub account with permission to create user-owned Projects. Sign in as `Doldas` and grant the GitHub CLI its **project** scope:

```sh
gh auth login
gh auth refresh -s project
python3 scripts/bootstrap-github-project.py
```

Run from a checkout that includes the helper (`release/2` after the bootstrap and fixes are merged). The script creates the Project if no Project with the exact title `Flowerly OS — Engineering Roadmap` exists. It reuses that Project on later runs, links it to `flowerly_os`, creates any missing planning fields/views, and imports issues and pull requests including closed items (up to 1,000 of each). Rerun to import newly created issues or PRs. Existing populated field values are left untouched; unset fields are initialized, including on items left partially imported by an interrupted run.

If an earlier run created project 7 and then failed, resume that exact board:

```sh
python3 scripts/bootstrap-github-project.py --project 7
```

Do not delete the partially created Project. The helper verifies its title before changing it, reuses existing fields case-insensitively, and reports incompatible field types or missing options rather than replacing them. Field errors include the attempted field name.

GitHub reserves **Type** for its built-in issue-type field, including when it is hidden. The bootstrap's separate custom category field is named **Work type**. An error from `createProjectV2Field` about a reserved/taken name in older bootstrap versions is addressed by updating the script; do not try to delete or recreate GitHub's built-in Type field.

## Project schema

- **Target release:** 0.0.2–0.0.9, 0.1.0, or Unscheduled
- **Delivery stage:** Backlog, Ready, In progress, Review, Done
- **Sprint:** free-text sprint association when present in an issue title
- **Area:** Core, Flow, Session, Storage, Evolution, Goal, Agent, Image, Docs, CI, Security, Planning, Other
- **Work type:** Roadmap, Feature, Bug, Security, Test, Docs, Research, Planning, Chore, Pull request (separate from GitHub's built-in Type)
- **Priority:** P0–P3 or Unassigned, based on issue labels
- **Risk:** initially Unassessed; human triage required
- **Owner:** derived from issue assignees when available

GitHub's built-in **Status** field remains available. The custom **Delivery stage** field preserves all five requested planning stages, without overwriting existing Status options.

The script also creates these native views: **Roadmap** (table), **Current Sprint** (board), **Release Board** (board), **Architecture / Research** (table), and **Bugs & Security** (table). Set per-view grouping, filters, and date columns in GitHub's Project UI after creation. In particular, group Roadmap by Target release and Release Board by Delivery stage; set Current Sprint to the current release (0.0.4). Date-based timelines require real target dates and must not be invented.

## Initial release mapping

| Target release | Tracking issue | Stage at preparation |
| --- | --- | --- |
| 0.0.2 Core Runtime Skeleton | #6 | Done |
| 0.0.3 Persistent State | #7 | Done |
| 0.0.4 Flows, Sessions & Runtime Hardening | #8, #31 | In progress |
| 0.0.5 Generations | #9 | Backlog |
| 0.0.6 Evolution Lab | #10, #3 | Backlog |
| 0.0.7 Goal Runtime | #11 | Backlog |
| 0.0.8 Agent Runtime | #12, #4 | Backlog |
| 0.0.9 Minimal Agentic Environment | #13 | Backlog |
| 0.1.0 First Real Agentic OS | #14 | Backlog |

Sprint #2 maps to 0.0.4; sprint #1 maps to 0.0.2 but **remains open** in GitHub and is marked Ready for triage rather than automatically completed. Issue #15 remains an administrative task until the Project is verified. Other issues and PRs with no reliable release designation use **Unscheduled**; the script does not guess milestone assignments.

The post-0.1 roadmap is directional only. Do not create fake delivery dates or duplicate issues for every sentence of the architecture roadmap. Detailed acceptance checks are kept in their release tracking issues and linked documentation until they become concrete, individually assignable work.

## Notes

- The script imports already-closed issues and PRs to preserve project history. Filter Delivery stage = Done out of active boards.
- Re-running fills only unset fields on imported items, preserving human-owned delivery stages and Risk assessments. GitHub Projects' built-in workflows can update status when issues close.
- Project creation uses the authenticated user's GitHub CLI permissions; the ChatGPT GitHub connector cannot perform Projects v2 mutations.

Offline regression checks (no GitHub credentials required):

```sh
python3 -m unittest discover -s scripts -p 'test_bootstrap_github_project.py' -v
```

