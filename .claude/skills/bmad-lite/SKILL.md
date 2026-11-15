---
name: bmad-lite
description: Token-conscious BMAD-style planning and execution for substantial changes; skip ceremony for small fixes.
---

This is a lightweight repository workflow, NOT the upstream BMad Method package. For official BMad v6.12, follow docs/agentic-coding/bmad.md (or web-docs/docs/agentic-coding/bmad.md in Flowerly OS).

Choose the smallest useful workflow:
- Trivial fix (single obvious change): implement directly with a focused test. Do not create planning documents or agents.
- Bounded feature (multiple files, clear design): concise task brief, then implement and test.
- Cross-module architecture/security/release change: brief + explicit risks, options, acceptance criteria and human approval before coding.

For larger tasks:
1. Verify branch/head, repository guidance, current relevant issue/PR, and scope; don't infer live state from past notes.
2. Read existing docs and use the Haiku Explore subagent for bounded discovery. Avoid duplicate scanning or loading all docs.
3. Save a concise brief to the existing project handoff path or a task-specific file, only if the work warrants durable context:
   - Goal and non-goals
   - Existing design and relevant files
   - Small sequenced implementation steps
   - Verification and acceptance checks
   - Risks and open decisions
4. Ask for a decision only where an unresolved high-impact trade-off requires it; otherwise proceed within the user's approved scope.
5. Implement one coherent slice in Sonnet, use targeted checks, and delegate large pre-existing log analysis to Haiku log-triage.
6. Maintain the handoff with verified changes, tests and next steps. Do not copy transcripts or huge logs.

Rules:
- Prefer a single focused agent over parallel agents unless tasks are independent.
- Do not assume a Haiku call is free; avoid spawning agents for trivial queries.
- Never weaken tests, permissions, approval controls, or branch policies to reduce tokens.
- BMAD artifacts are optional; never add a new planning file for every small edit.
- No auto-merge or deployment unless explicitly authorized.
