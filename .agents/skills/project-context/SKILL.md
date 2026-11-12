
---
name: project-context
description: Restore, checkpoint, and hand off llm-proxy development context. Use when resuming work, switching sessions, completing milestones, or updating implementation decisions.
---

# Project Context Management

## Sources of Truth

Read only what is needed, in this order:

1. `docs/implementation-progress.md` — current state, evidence, blockers, next steps.
2. `docs/architecture.md` — design and invariants relevant to the task.
3. `docs/superpowers/plans/2026-11-12-llm-proxy-v1.md` — approved scope and milestones.
4. `docs/conventions.md` — applicable development conventions.

Repository state and verified results take precedence over stale summaries.
Do not treat plans or unchecked progress claims as completed work.

## Resume

1. Read the latest progress checkpoint.
2. Inspect `git status` and relevant changes without modifying them.
3. Identify the current milestone, last verified result, unfinished work, and blockers.
4. Verify uncertain claims against relevant code, tests, or logs.
5. Continue from the next unfinished task. Avoid repeating verified work.

## Checkpoint

After each milestone or material decision change, update
`docs/implementation-progress.md` with:

- Current milestone and status.
- Completed changes and affected modules.
- Verification commands and actual PASS/FAIL/BLOCKED results.
- Decisions and rationale.
- Known issues, blockers, and risks.
- Next concrete actions.

Record facts, not assumptions. Never claim unexecuted tests passed.
Preserve useful prior decisions and evidence; consolidate obsolete notes
without erasing unresolved issues.

## Handoff

Before ending substantial work:

1. Save a concise checkpoint if progress or decisions changed.
2. Ensure the next task is actionable without chat history.
3. Highlight uncommitted changes and unverified behavior.
4. Summarize completed, verified, blocked, and remaining work.
5. Never claim live integration was tested using only mocks.

## Constraints

- Follow root `AGENTS.md`; do not override its invariants.
- Do not duplicate architecture or plans into progress notes.
- Do not store secrets, credentials, tokens, or sensitive payloads.
- Do not overwrite unrelated user changes.
- Do not push, publish, deploy, or run live authenticated tests without authorization.
- Prefer concise, structured checkpoints over conversation transcripts.