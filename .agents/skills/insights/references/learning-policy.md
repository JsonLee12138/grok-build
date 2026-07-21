# Learning Policy

VibeRig learns only from accepted work.

## Allowed Auto-Apply

Only auto-apply a candidate when all are true:

- `type` is `project_note`
- confidence is `high`
- evidence includes an accepted task or accepted planning gate
- the note is factual and low-risk
- the target is a Linear retrospective/proof comment, or an explicitly approved project-local learning file

Examples:

- The project validation command is `go test ./...`.
- The accepted workflow uses `origin/main` as the default base.

## Requires Confirmation

Always require user confirmation for:

- `workflow_rule`
- `skill_update`
- `user_preference`
- any candidate with `medium` or `low` confidence
- any candidate that changes future agent behavior beyond the current project
- any candidate that modifies `skills/*/SKILL.md`, `AGENTS.md`, `.codex/agents`, or user memory

## Do Not Learn

Do not create candidates from:

- failed attempts that were abandoned
- transient environment failures
- missing credentials
- uninstalled tools
- one-off task narratives
- unmerged or unaccepted code
- claims that a tool is broken
- speculative preferences
- early implementation paths later replaced by the accepted solution

## Development-Time Use

During implementation, read confirmed learnings as guidance when the project has an approved learning location. Do not update learning files or Linear retrospective comments until the post-acceptance finalizer runs.
