# Project & Release Roadmap

This page is the public planning overview. The detailed acceptance criteria remain in [Roadmap](../roadmap.md), and release policy is defined in [Releases](../releases.md).

| Release | Theme | Status | Tracking |
| --- | --- | --- | --- |
| 0.0.1 | Repository Foundation | Done | — |
| 0.0.2 | Core Runtime Skeleton | Mostly done | [#6](https://github.com/Doldas/flowerly_os/issues/6) |
| 0.0.3 | Persistent State | Next | [#7](https://github.com/Doldas/flowerly_os/issues/7) |
| 0.0.4 | Flows and Sessions | Planned | [#8](https://github.com/Doldas/flowerly_os/issues/8) |
| 0.0.5 | Generations | Planned | [#9](https://github.com/Doldas/flowerly_os/issues/9) |
| 0.0.6 | Evolution Lab | Planned | [#10](https://github.com/Doldas/flowerly_os/issues/10) |
| 0.0.7 | Goal Runtime | Planned | [#11](https://github.com/Doldas/flowerly_os/issues/11) |
| 0.0.8 | Agent Runtime | Planned | [#12](https://github.com/Doldas/flowerly_os/issues/12) |
| 0.0.9 | Minimal Agentic Environment | Planned | [#13](https://github.com/Doldas/flowerly_os/issues/13) |
| 0.1.0 | First Real Agentic OS | Planned | [#14](https://github.com/Doldas/flowerly_os/issues/14) |

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
