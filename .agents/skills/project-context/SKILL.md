---
name: project-context
description: Restore, checkpoint, and hand off llm-proxy development context.
---

# Project context checklist

Follow `AGENTS.md`; read progress → architecture → approved v1 plan → conventions.
Read task-relevant source only after that; repository state/evidence beats stale notes.

## Resume

1. Inspect `git status` and relevant changes without overwriting user work.
2. Identify current milestone, last verified results, blockers and next unfinished task.
3. Verify uncertain claims against code/tests/logs; do not repeat settled work needlessly.

## Checkpoint / handoff

Update `docs/implementation-progress.md` after every milestone/material decision:
- Current implementation and decisions, with a brief rationale where needed.
- Actual checks and PASS/FAIL/BLOCKED results; distinguish mocks from live acceptance.
- Uncommitted work, unresolved risks/blockers and the next concrete actions.

Keep one concise current checkpoint, not a conversation transcript. Preserve unresolved
issues and useful evidence; use Git for historical detail and architecture for durable
invariants. Never record secrets/payloads or claim unexecuted checks passed. Follow
`AGENTS.md` authorization limits for credentials, live tests, deployment and Git pushes.
