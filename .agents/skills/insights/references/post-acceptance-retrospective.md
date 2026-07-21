# Post-Acceptance Retrospective

Use this flow only after the accepted-work gate passes.

Render human-facing retrospective prompts, summaries, and reports in
`.vibeRig/project.yaml` `output.language`. Keep evidence-bundle YAML keys,
candidate type names, stable IDs, local paths, commands, branch names, Linear
keys, acceptance IDs, schema keys, and code symbols unchanged.

## Required Gate

At least one accepted signal must be present:

- Linear issue moved to the team's accepted/done state after validation
- proof packet comment shows validation passed and reviewer accepted
- PR merged or human handoff accepted

Do not run this retrospective for active implementation or speculative planning.

## Evidence Bundle

Expected fields:

```yaml
requirement_id: VB-123
linear_issue: VB-456
status: accepted
gate:
  validation_passed: true
  acceptance_passed: true
  code_review_passed: true
sources:
  brief: .vibeRig/requirements/VB-123/brief.md
  contract: .vibeRig/requirements/VB-123/contract.json
  architecture: .vibeRig/requirements/VB-123/architecture.md
  acceptance: .vibeRig/requirements/VB-123/acceptance.md
  validation: .vibeRig/requirements/VB-123/validation.md
linear:
  proof_packet_comment: "<url-or-comment-id>"
  issue_url: "<url>"
git:
  branch: "<branch>"
  head_sha: "<sha>"
  changed_files: []
validation:
  commands: []
  result: passed
review:
  acceptance_notes: ""
  code_review_notes: ""
```

## Finalizer Prompt

```text
Review only accepted work.

Use the provided evidence:
- Linear issue and proof packet comment
- local Docs as Code requirement contract
- implementation diff
- validation results
- review notes

Extract durable learnings only if they are proven by the accepted outcome.

Do not learn from:
- failed attempts that were abandoned
- transient environment issues
- unmerged code
- one-off task details
- speculative preferences

Classify each candidate as:
project_note, workflow_rule, skill_update, or user_preference.

When the accepted evidence suggests a reusable skill-library change, also run
`skillos-lite` to emit curation proposals. Classify each proposal as
`insert`, `update`, `deprecate`, or `noop`. Do not apply curation proposals
without explicit user confirmation, and route confirmed skill changes through
`skill-builder`.

Auto-apply only low-risk high-confidence project notes.
For workflow rules, skill updates, and user preferences, produce a proposal unless the policy explicitly allows automatic application.

If there is no durable learning, output:
Nothing to save.
```
