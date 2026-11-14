---
name: context-handoff
description: Create a compact, persistent task handoff for a new Claude Code conversation, using only verified facts.
---

Produce or update docs/agent-context/active-handoff.md when the user asks for a handoff or a task has reached a natural stopping point.

Read the current branch, changed files and relevant existing handoff before writing. Do not infer unverified CI status. Never treat old handoff notes as more authoritative than Git, code or current CI.

Use these headings:
- Objective and acceptance criteria
- Branch and verified commit
- Work completed (with file paths)
- Test/CI evidence (exact commands, outcomes, and any missing checks)
- Open questions or risks
- Next smallest steps

Keep the document concise (ideally 200–400 words). Do not copy chat transcripts, raw logs, secrets or large code snippets. Update existing notes instead of appending endlessly. Do not commit or push without user instruction. 
