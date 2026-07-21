---
name: <skill-name>
description: Use when <specific user intents, task contexts, symptoms, file types, tools, or risk signals>. Include key terms users naturally say. Do not summarize the workflow.
---

# <Skill Name>

## Contract

Use this skill to <single responsibility — one sentence>.

Do not use this skill for:
- <adjacent task that belongs to another skill — name that skill> mid-task; redirect to `<other-skill>` if this comes up during execution.
- <second clear exclusion>.

Stop and report when <blocking condition that requires human input or is out of scope>.

## Input Contract

Required:
- <file / path / tool / context the skill cannot proceed without>
- <acceptance criteria or target behavior>

Optional:
- <logs, diffs, screenshots, or configs that improve quality but are not blocking>

If required inputs are missing, <inspect and infer / ask the single most blocking question / stop>.

## Output Contract

Return or produce:
- <primary artifact or change>
- <verification evidence: commands run, pass/fail, key errors>
- <residual risks or skipped checks>

Do not claim completion unless <minimum evidence — e.g., "tests pass and no placeholder remains">.

## Workflow

1. Read existing files and local patterns before making any change.
2. Identify scope, constraints, and files that must not be touched.
3. Load only the reference files needed for this path (see Context Loading).
4. Execute the smallest scoped change or analysis.
5. Validate using the Validation checklist below.
6. If validation fails: identify root cause → form hypothesis → fix → re-validate.
7. Report concise evidence matching the Output Contract.

## Context Loading

<!-- Remove this section if the skill has no references/ or assets/. -->

Read only when needed:
- `references/<topic>.md`: read when <condition>.
- `assets/<template>`: copy and fill when creating <artifact>.

Do not load all references at the start.

## Scripts

<!-- Remove this section if the skill has no scripts/. -->

Available:
- `scripts/<name>.py`: <purpose>. Run `python scripts/<name>.py --help`.

Execute scripts rather than reimplementing their logic.

## Red Flags

Observable signs the skill is being violated during execution — stop and correct before continuing:

- <symptom observable during execution> → <what it means and what to do instead>
- <second symptom> → <correction>

## Anti-Rationalization

| Rationalization | Reality |
|---|---|
| "<excuse to skip a workflow step>" | "<why the shortcut fails and what the real risk is>" |
| "<second excuse>" | "<rebuttal>" |

## Validation

Minimum checks before reporting completion:

```bash
# <what this checks>
<executable command>   # expected: <pass condition>
```

- [ ] <second verifiable criterion — e.g., "no placeholder remains outside marked examples">
- [ ] <third criterion — e.g., "all referenced files exist">

If a check cannot run, explain why and state the residual risk.
