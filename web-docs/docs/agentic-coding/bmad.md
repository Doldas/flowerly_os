# Lightweight BMAD integration for Claude Code

This repository includes a **BMAD-inspired low-token workflow** in `.claude/skills/bmad-lite/SKILL.md`. It is usable without installing third-party software. **The upstream BMad Method runtime is not yet installed by this PR.**

## Model and context policy

- Use Sonnet for reasoning about implementation, code changes, and final verification.
- Use the existing Haiku `Explore` agent for codebase mapping only when discovery is substantive; use `log-triage` for existing long error logs.
- Keep a task brief under ~400 words; don't paste full specs into every prompt.
- Restart unrelated work in a fresh context; preserve only verified handoff facts.
- Avoid simultaneous speculative agent explorations and large MCP tool inventories.

## When to use BMAD

| Scope | Workflow |
| --- | --- |
| One obvious bug | Direct edit and targeted test; no BMAD artifacts |
| Multi-file feature | `/bmad-lite` for a brief, implementation, focused checks |
| Architecture/security/release changes | `/bmad-lite` with alternatives, risks, acceptance gates, then human review |

Project-specific guardrails: Flowerly OS must follow `AGENTS.md`, live release/2 issue/PR states and required preflight/CI. Treat every privileged runtime boundary as security-sensitive. Never automatically merge a release PR.

## Optional: install official stable BMad Method v6.12

On the **machine where Claude Code runs**, from this repository root:

```sh
npx bmad-method install
```

Prerequisites: Node.js 20.12+, Python 3.10+ and uv; check upstream before proceeding. In the interactive installer select **Claude Code**, a minimal set of modules, and inspect changes before committing. Avoid installing every optional agent.

Upstream docs: https://docs.bmad-method.org/start/install-bmad/

The upstream installer may generate `_bmad/`, `_bmad-output/`, and/or skills depending on selected version. Review all generated files and any new prompts for overlapping agent instructions before committing. Do not assume this PR installed that runtime. Avoid the v7 preview for a stability-focused rollout until you intentionally choose it.

## Quick trial

1. Open a fresh Claude Code session in this repo.
2. Run `/agents` and verify `Explore` / `log-triage` appear.
3. Invoke `/bmad-lite` for a multi-file task and verify that it makes a short brief without broad scanning.
4. Compare `/context` and usage reports for similar tasks before/after.
