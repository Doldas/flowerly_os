# Claude Code navigation — Flowerly OS

Read `AGENTS.md` first and follow its release, safety, branch, CI, and human-approval requirements. This file does not override those rules.

## Efficient navigation
- Main components: `src/flowerlyd/`, `src/evolutiond/`, `src/shared/flowerly-core/`, `tools/`, `distro/`, `testbed/`; architecture and release docs in `web-docs/docs/`.
- Consult `web-docs/docs/release-0.0.4-completion-plan.md` and live GitHub issue/PR/CI state for release tasks; never assume the handoff is current.
- Delegate nontrivial read-only searches to the project `Explore` Haiku agent; use focused symbol/path searches rather than whole-file dumps.
- Use `log-triage` for long existing compiler/CI logs, but verify critical failures against the source evidence.
- Keep output small. Avoid printing full Cargo logs, lockfiles, generated artifacts, or entire Rust modules.
- Favor `cargo test -p <package>` or targeted tests while iterating; never skip mandatory preflight or release CI gates. Before pushing a PR update, follow `AGENTS.md` and run `bash scripts/preflight-docs.sh`.
- Before the next session, use `/context-handoff` and consult `web-docs/docs/agent-context/active-handoff.md` only as a hint; revalidate branch/head and CI.

## Safety
- Preserve PTY/cgroup isolation, authenticated IPC, SQLite transaction and retry invariants.
- No automatic merges or master promotion; leave approval to the maintainer.
