# Project & Release Roadmap

This page is the public planning overview. The detailed acceptance criteria remain in [Roadmap](../roadmap.md), release policy is defined in [Releases](../releases.md), and the review-derived implementation order is preserved in the [Review-driven implementation plan](../review-implementation-plan-2026-10-12.md).

| Release | Theme | Status | Tracking |
| --- | --- | --- | --- |
| 0.0.1 | Repository Foundation | Done | — |
| 0.0.2 | Core Runtime Skeleton | Done | [#6](https://github.com/Doldas/flowerly_os/issues/6) |
| 0.0.3 | Persistent State | Done | [#7](https://github.com/Doldas/flowerly_os/issues/7) |
| 0.0.4 | Flows, Sessions & Runtime Hardening | In progress | [#8](https://github.com/Doldas/flowerly_os/issues/8) |
| 0.0.5 | Generations | Planned | [#9](https://github.com/Doldas/flowerly_os/issues/9) |
| 0.0.6 | Evolution Lab | Planned | [#10](https://github.com/Doldas/flowerly_os/issues/10) |
| 0.0.7 | Goal Runtime | Planned | [#11](https://github.com/Doldas/flowerly_os/issues/11) |
| 0.0.8 | Agent Runtime | Planned | [#12](https://github.com/Doldas/flowerly_os/issues/12) |
| 0.0.9 | Minimal Agentic Environment | Planned | [#13](https://github.com/Doldas/flowerly_os/issues/13) |
| 0.1.0 | First Real Agentic OS | Planned | [#14](https://github.com/Doldas/flowerly_os/issues/14) |

## Distribution substrate

Arch Linux is the selected initial userspace/package substrate. The rolling
upstream feeds candidate generations; Flowerly Stable World changes only through
tested promotion. This affects the Generations, Evolution Lab and Minimal
Agentic Environment milestones. See
[Image & Userspace Model](../image-model.md) and
[ADR-0015](../adr/0015-arch-linux-userspace-substrate.md).

## Post-0.1 direction

The current long-range themes are:

1. **0.2 — Flow OS**
2. **0.3 — Adaptive Agentic Shell**
3. **0.4 — Ambient Interaction**
4. **0.5 — User Model**
5. **0.6 — Autonomous Evolution**
6. **0.7 — Multi-machine Agentic OS**
7. **1.0 — Daily-drivable self-evolving Flowerly OS**

These post-0.1 themes are directional rather than fixed contracts. Evidence from earlier releases should shape exact scope.

## GitHub Project model

The recommended GitHub Project views are:

- **Roadmap** — grouped by target release.
- **Current Sprint** — filtered to active work.
- **Release Board** — grouped by status: Backlog, Ready, In progress, Review, Done.
- **Architecture / Research** — ADR, RFC and research issues.
- **Bugs & Security** — defects and security work separated from feature planning.

Recommended Project fields:

```text
Status
Target release
Sprint
Area
Type
Priority
Risk
Owner
```

Each release tracking issue should link back to the authoritative roadmap section and contain a checklist of its exit criteria.


## Native GitHub Project setup

The [Project bootstrap guide](bootstrap.md) and
[`scripts/bootstrap-github-project.py`](../../scripts/bootstrap-github-project.py)
create a real GitHub Projects v2 board and import all issues and pull requests,
including historical closed items. The helper is idempotent: re-running it
does not overwrite already-imported items.

The current active release is **0.0.4**. The earlier proposed statuses in
[issue #15](https://github.com/Doldas/flowerly_os/issues/15) predate the
completion of 0.0.2 and 0.0.3 and must not be reused.
