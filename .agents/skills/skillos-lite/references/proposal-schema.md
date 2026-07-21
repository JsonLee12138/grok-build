# SkillOS Lite Proposal Schema

Render human-facing explanations in `.vibeRig/project.yaml` `output.language`.
Keep operation names, schema keys, skill directory names, file paths, Linear keys,
commit hashes, commands, and code symbols unchanged.

## JSON Shape

```json
{
  "curation_proposals": [
    {
      "id": "CUR-001",
      "operation": "update",
      "target_skill": "task-runner",
      "change_summary": "Require proof packets to include the exact validation command output source.",
      "evidence": [
        "Linear ABC-123 human acceptance comment",
        "Proof Packet comment <id-or-url>",
        "PR <url>"
      ],
      "confidence": "medium",
      "risk": "low",
      "validation_plan": "Use skill-builder to update the Validation or Output Contract section, then verify frontmatter and referenced files.",
      "requires_confirmation": true,
      "notes": "Optional reviewer-facing rationale."
    }
  ]
}
```

## Field Rules

- `id`: stable within the report, using `CUR-###`.
- `operation`: one of `insert`, `update`, `deprecate`, or `noop`.
- `target_skill`: skill directory name under `skills/`, proposed new skill directory name, or empty for `noop`.
- `change_summary`: one sentence describing what should change and why.
- `evidence`: accepted-work evidence references, not general conversation memory.
- `confidence`: one of `high`, `medium`, or `low`.
- `risk`: one of `low`, `medium`, or `high`.
- `validation_plan`: concrete validation for `skill-builder` or human review.
- `requires_confirmation`: `true` for `insert`, `update`, and `deprecate`; `false` for `noop`.
- `notes`: optional rationale or caveat.

## Noop Example

```json
{
  "curation_proposals": [
    {
      "id": "CUR-001",
      "operation": "noop",
      "target_skill": "",
      "change_summary": "No durable skill curation is justified from this accepted work.",
      "evidence": [
        "Linear ABC-123 human acceptance comment"
      ],
      "confidence": "high",
      "risk": "low",
      "validation_plan": "No skill-builder action required.",
      "requires_confirmation": false
    }
  ]
}
```
