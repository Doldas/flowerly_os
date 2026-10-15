# Agent and contributor handoff

This file provides **navigation and accuracy rules**, not authority to override repository code, security checks or the human maintainer's instructions. The GitHub issue and actual branch/code state are the source of truth; verify them anew each session.

## Development workflow

- Active 0.0.4 integration target: `release/2`. Fetch its **current** SHA, active issues, PR states and CI before implementing anything.
- Do not merge implementation PRs unless the human explicitly authorizes it. Release-to-release forwarding may use GitHub's protected auto-merge **only after required checks** when explicitly configured. Master promotion is **never** auto-merged, even after tests pass. Use scoped feature/docs branches, draft PRs, CI and real black-box evidence.
- Record material architecture decisions, failure findings, unclosed questions and reproducible acceptance tests in repository docs/issues. Next agent sessions must not depend on the previous chat's context.
- For long-lived runtime and process behaviors, trust measured Linux process behavior over simulated state. Protect safety-critical invariants with failure-injection tests.

## Consolidated architecture migration

- Issue [#105](https://github.com/Doldas/flowerly_os/issues/105) tracks the distribution-first structural transition.
- Runtime packages: `src/flowerlyd`, `src/evolutiond`, `src/shared/flowerly-core`.
- Build and distribution inputs: `tools/`, `distro/`, `testbed/`; documentation: `web-docs/docs/`.
- Do not reintroduce legacy state daemon binaries or inter-module Unix sockets.
- Preserve authenticated IPC, PTY/cgroup containment, transactional SQLite semantics and unknown-outcome retry safety. Treat incomplete integration tests as merge blockers.
- Hypervisor provisioning and bootable image builds remain explicitly unavailable, even when scaffolds exist.

## Release propagation policy

- Issue #112 tracks release branch forwarding. `branch-forward.yml` reconciles
  adjacent `release/N → release/N+1` PRs and a draft latest release → master
  candidate; no direct branch writes.
- A separate `FLOWERLY_FORWARD_BOT_TOKEN` secret is required for bot-created
  PRs to trigger required checks. Repository "Allow auto-merge" must also
  be enabled for unattended adjacent-release merges; absent setup is a
  visible configuration blocker.
- Exact-SHA successful **push** CI including real Arch VM acceptance, security,
  and CodeQL are required for any master PR. Master always needs maintainer
  approval and manual merge. See [release model](web-docs/docs/releases.md).

## Present release versus proposals

- Milestone 0.0.4: [roadmap issue #8](https://github.com/Doldas/flowerly_os/issues/8), [hardening gates #31](https://github.com/Doldas/flowerly_os/issues/31), [PTY supervisor #50](https://github.com/Doldas/flowerly_os/issues/50), [required release CI #55](https://github.com/Doldas/flowerly_os/issues/55), [SQLite contention #56](https://github.com/Doldas/flowerly_os/issues/56), [typed SQLite errors #62](https://github.com/Doldas/flowerly_os/issues/62) and [unknown-outcome retries #63](https://github.com/Doldas/flowerly_os/issues/63).
- PRs #60 and #61 were verified merged when this document was written. **Recheck** current repository and CI, do not blindly rely on that snapshot.
- Production `sessiond` PTY ownership, authenticated cross-device remote session access and complete Flow A → B → A real-process reattach have **not** been verified completed. The Python PTY spikes #57–#59 and RFC 0003 are experiments/proposals, not a production runtime.
- The [location-independent architecture RFC 0004](docs/rfcs/0004-location-independent-flowerly-ai-compute.md) and [planning issue #64](https://github.com/Doldas/flowerly_os/issues/64) describe **future, unapproved possibilities**: independent client/runtime/inference placement, local or external model providers, expert AI profiles, remote/VM browser clients, and optional enterprise centralization. Do **not** add them to 0.0.4 scope.
- A browser is a client, not a VM. Provider/model selection cannot grant OS permissions. Local/offline administration cannot require AI connectivity. Organization resource policy and identity boundaries must be enforced independently of agent preferences.

When a future session discovers new risks, check existing issue coverage before creating a duplicate; cite evidence and distinguish observed defects from inferred risks.
