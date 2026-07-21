---
name: skill-builder
description: Create or update Agent Skills/Codex skills with reliable trigger descriptions, concise SKILL.md workflows, progressive disclosure, reusable scripts/references/assets, and validation checklists. Use when the user asks to design, create, refine, audit, or troubleshoot a skill, SKILL.md file, or reusable agent workflow package.
---

# Skill Builder

Create skills as focused, reusable workflow packages. A good skill is not a long prompt; it is a small playbook with a precise trigger, clear execution contract, optional reusable resources, and validation evidence.

## Contract

Use this skill when creating, updating, reviewing, or troubleshooting an Agent Skills-compatible `SKILL.md` package.

Do not use this skill for:
- General prompt writing that will not become a reusable skill.
- Creating custom subagent TOML files mid-task; redirect to `agent-creator` if this comes up during execution.
- Installing, publishing, or globally syncing skills unless the user explicitly asks.

Stop and ask only when the skill's target job, target location, or required proprietary/source material cannot be inferred safely. Otherwise make conservative assumptions and proceed.

## Input Contract

Prefer these inputs:
- Skill name or intended capability.
- Target directory, such as `skills/`, `.codex/skills/`, or `~/.codex/skills/`.
- Real user requests that should trigger the skill.
- Real user requests that should not trigger the skill.
- Existing docs, code, runbooks, templates, scripts, or previous failures to encode.

If examples are missing, draft 3-5 realistic trigger examples from the user's goal and use them to shape the skill. Do not block on examples unless the domain is high risk or proprietary details are required.

## Output Contract

Produce the skill files directly when the user asks to create or update a skill. Return:
- Changed file paths.
- The trigger intent captured in `description`.
- Any resources intentionally added or omitted.
- Template files added, reused, or intentionally kept inline, with the reason.
- Validation performed and remaining gaps.

Do not claim the skill is complete unless the `SKILL.md` frontmatter is valid, the body has no unresolved placeholders, and the skill can be discovered from the requested location.

Literal placeholders are allowed only inside fenced examples or templates that are clearly marked as examples. Do not leave placeholders in executable instructions, required commands, file paths that should exist, or final user-facing artifacts.

## Workflow

1. Inspect local conventions.
   - Check nearby skills for naming, directory structure, metadata, and optional `agents/openai.yaml` style.
   - Preserve local conventions unless they conflict with the Agent Skills format.
2. Define the reusable job.
   - State one primary responsibility.
   - Identify clear exclusions so the skill does not become a catch-all workflow.
3. **Write and validate the trigger first — before writing the body.**
   - Put all activation guidance in frontmatter `description`.
   - Describe user intent, task contexts, symptoms, file types, tools, and risk signals.
   - Include key terms users naturally say.
   - Do not summarize the whole internal workflow in the description.
   - Mentally test against Trigger Quality Checks (see below) before moving on.
4. Choose the resource plan.
   - Keep core workflow, critical gotchas, and validation rules in `SKILL.md`.
   - Put detailed API docs, framework variants, schemas, and long examples in one-level `references/` files.
   - Put deterministic or repeatedly rewritten logic in `scripts/`.
   - Put output templates, boilerplate, images, fonts, or copied files in `assets/`.
   - Move reusable Markdown/YAML/JSON templates longer than a short illustrative snippet into `assets/` or `references/`.
   - In `SKILL.md`, link the template and explain when to read or fill it; do not inline the whole reusable artifact.
   - Do not add README, install guides, changelogs, or process notes unless the user explicitly requires them.
5. Write `SKILL.md`.
   - Keep it concise and imperative.
   - Prefer concrete procedures, defaults, and checklists over broad principles.
   - Match specificity to risk: flexible guidance for judgement tasks, exact commands/scripts for fragile operations.
6. Validate.
   - Run the Validation checklist below before reporting completion.

## Trigger Quality Checks

Test the `description` against these before writing the body:

**Should trigger** — mentally run 3-5 prompts that clearly need this skill and confirm the description would catch them.

**Should not trigger** — mentally run 3-5 adjacent prompts that belong elsewhere and confirm the description would not capture them.

Revise the description if it is too broad, too vague, or missing words users would naturally say.

## Recommended SKILL.md Shape

Use this structure for development-oriented skills unless local convention suggests a better fit: [skill template](./assets/skill-template.md)

## Development Skill Rules

For skills that guide coding agents:
- Require the agent to read the codebase before editing.
- Require following existing project patterns, helper APIs, dependency choices, and test style.
- Protect unrelated user changes; never instruct broad reverts.
- Require an explicit test decision rather than forcing tests for every task.
- Require evidence: commands run, pass/fail state, key errors, skipped checks, and residual risk.
- Prefer scoped changes over opportunistic refactors.
- For high-risk work, use plan-validate-execute: write a short plan or mapping, validate it against the source of truth, then execute.

## Red Flags

Observable signs that the skill being built is violating good practice — fix before finishing:

- `SKILL.md` exceeds ~200 lines → extract to `references/` or split into two skills.
- `description` exceeds 3 lines → it is describing the workflow, not the trigger; trim to intent signals only.
- The body says "see references for details" but no `references/` file exists.
- A reusable template (>20 lines) is inlined in `SKILL.md` instead of living in `assets/`.
- Validation section has only "confirm X" phrasing with no commands or exit criteria.
- The skill handles more than one primary responsibility (two unrelated `##` workflow branches).
- `references/` files link to other `references/` files (deep chain).

## Anti-Rationalization

Common shortcuts you might reach for — and why they fail:

| Rationalization | Reality |
|---|---|
| "The description is clear enough to me" | If it is clear to you now, test it against what a new user would type in 3 months. Generic words like "create", "manage", or "handle" do not survive that test. |
| "This SKILL.md is a bit long but it covers everything" | Length is a signal the skill has multiple jobs or the references are not extracted. A 300-line SKILL.md is two skills or one skill plus a missing `references/` file. |
| "The template is short enough to inline" | Inline templates grow. The moment anyone edits `SKILL.md` to fix the template, the skill and template diverge. Move it to `assets/` now. |
| "I'll skip Validation — the file looks fine" | Looking fine and being discoverable, placeholder-free, and linked correctly are different things. The checklist catches the class of errors you cannot see by reading. |
| "The user didn't give me trigger examples, I'll skip that check" | Trigger quality is the single most important property of a skill. Without examples, draft 3-5 from the goal and test the description against them before writing anything else. |
| "Common Mistakes covers the pitfalls, I don't need Red Flags" | Common Mistakes are for skill users at runtime. Red Flags are for you, right now, during authoring. They are different audiences. |

## Validation

Minimum validation for every skill change:

```bash
# 1. Frontmatter has name and description
grep -E "^name:|^description:" SKILL.md | wc -l   # must be 2

# 2. Directory name matches skill name
dir=$(basename $(pwd)); grep "^name:" SKILL.md | grep -q "$dir" && echo "ok" || echo "MISMATCH"

# 3. No unresolved placeholders outside example blocks
grep -n "<[a-z]" SKILL.md | grep -v "^\s*[-*].*example\|fenced\|template"

```

- Confirm the skill has a clear contract, input contract, output contract, workflow, and validation guidance, or document why local convention intentionally differs.
- Confirm references, scripts, and assets mentioned from `SKILL.md` exist.
- Confirm placeholders exist only inside explicitly marked examples/templates.
- Confirm every referenced template file exists and is directly linked from `SKILL.md`.
- Confirm `SKILL.md` states when each template should be read, copied, or filled.
- Mentally test 3 should-trigger and 3 should-not-trigger prompts against the description.
