---
name: insights
description: Generate VibeRig post-acceptance retrospectives, learning candidates, and SkillOS-lite curation proposals from accepted Linear work. Use after validation, acceptance review, and code review pass; when asked to summarize accepted VibeRig work; or when applying confirmed learning candidates. Do not use during active implementation except to read confirmed project learnings.
---

# Insights

Use this skill to turn accepted VibeRig work into conservative learning candidates and skill curation proposals.

## 与 vb-learn 的分工（里程碑原生工作流）

两个 skill 是上下游，不重复：

- **insights（复盘）**：从已验收的证据生成复盘结论与学习候选，**必须通过 `vb-linear` 把复盘结论写入 Linear 评论区**（issue 级写在 issue 上，里程碑级写在 Milestone 验收评论之后）。评论区既给人看，也是 `vb-learn` 的学习输入。
- **vb-learn（自学习）**：读评论区（复盘评论 + proof packet + 验收评论），提炼可泛化模式，经 `skill-builder` 沉淀为 `~/.vb-skills` 的 skill。
- 边界：insights **不直接沉淀 skill 到 `~/.vb-skills`**（那是 vb-learn 的职责；insights 只在用户明确确认某个 `skill_update` 候选时经 skill-builder 修改本仓库 `skills/`）；vb-learn 不自己写复盘评论。
- 调用时机：`accept-issue` 与 `accept-milestone` 验收通过后都先触发 insights 复盘、再触发 vb-learn 自学习。

Linear comments and local Docs as Code are the primary evidence sources. VibeRig no longer relies on local task-engine evidence directories or local proof packet files.

## Contract

Use this skill to generate or apply conservative learning candidates from accepted VibeRig work.

Do not use this skill for active implementation, task execution, final acceptance, or unreviewed memory capture. Use `task-runner` for execution and `accept-issue`/`accept-milestone` for sign-off.

Stop before applying a learning when evidence is not tied to accepted work, the user has not confirmed the candidate, or the change would modify a skill without routing through `skill-builder`.

## Operating Boundary

- Do not learn from all conversations.
- Do not learn during active implementation.
- Do not learn from failed, abandoned, unmerged, or unaccepted work.
- Do not modify other skills or user memory unless the user explicitly confirms.
- For every confirmed `skill_update` or SkillOS-lite curation proposal that adds, changes, or deprecates a skill package, invoke `skill-builder` first and follow its contract for trigger wording, resource planning, file edits, and validation.
- When called from `accept-issue` or `accept-milestone`, use the explicit human acceptance comment as the acceptance boundary.
- During implementation, read confirmed project learnings when present, but do not write new learnings.

## Input Contract

Required:

- Accepted Linear issue, PR, commit, or explicit user request for retrospective/application.
- Evidence that the work was accepted or the user explicitly authorized retrospective generation.

Optional:

- Requirement docs, validation logs, CI links, screenshots, review notes, proof packet comments, or confirmed learning candidates.

If acceptance evidence is missing, produce no durable learning and ask for the missing acceptance boundary.

## Valid Triggers

Use this skill when one of these is true:

- A Linear issue or sub-issue has passed validation and review.
- A PR or handoff tied to a Linear issue was accepted.
- `accept-issue` or `accept-milestone` has recorded explicit acceptance and requests post-acceptance insights.
- The user asks for a VibeRig retrospective, accepted-work insights, or learning candidates.
- The user asks to apply reviewed learning candidates.

## Evidence Sources

Prefer explicit evidence over conversation memory:

聚合粒度（里程碑原生工作流）：**以里程碑为单位**复盘——一个里程碑验收后聚合它名下全部 issue 的证据；需求级复盘 = 各里程碑复盘的汇总（最后一个里程碑验收归档时触发）。

1. Linear Milestone（该里程碑下全部 issue 的描述、状态、评论、proof packet、验收评论）与 PR/commit。
2. `.vibeRig/requirements/{requirement-id}/requirement.yaml`（里程碑列表与四态）
3. `.vibeRig/requirements/{requirement-id}/intake.md`
4. `.vibeRig/requirements/{requirement-id}/architecture.md`
5. `.vibeRig/requirements/{requirement-id}/acceptance.json`
6. Git diff, commits, PR description, and changed files
7. Validation output, CI URLs, screenshots, logs, and manual review notes

## Language Policy

Read `.vibeRig/project.yaml` and use `output.language` for human-facing retrospectives and learning reports.

- Linear retrospective comments, user-facing summaries, Linear documents created by this workflow, and local learning reports should use `output.language`.
- If `output.language` is missing, infer the language from the user's current working language, state the fallback, and recommend reconciling `project.yaml` through `init-viberig`.
- Do not translate stable IDs, file paths, commands, branch names, PR URLs, commit hashes, Linear keys, acceptance IDs, schema field names, code symbols, candidate type names, or existing external labels/status names.

## Workflow

1. Confirm the work is accepted or explicitly authorized for retrospective generation. Read `references/post-acceptance-retrospective.md` and follow its gate and evidence-bundle flow.
2. Read `.vibeRig/project.yaml`, including `output.language`, and the referenced requirement docs.
3. Read Linear proof packet comments and linked implementation evidence — ask `vb-linear` for the issue's status, description, branch, links, relations, and Proof Packet/reviewer handoff comments.
4. Optionally use `subagent-routing` for independent review or domain-specific insight analysis.
5. Generate learning candidates:
   - project facts and commands
   - validation improvements
   - acceptance gaps
   - architecture or process risks
   - proposed skill/rule updates
6. Use `skillos-lite` to generate skill curation proposals when accepted evidence suggests a reusable skill change, skill split, deprecation, or no-op decision.
7. Auto-apply only candidates that pass every rule in `references/learning-policy.md`. When in doubt, do not auto-apply.
8. Leave workflow rules, skill updates, curation proposals, and user preferences for explicit user confirmation.
9. When applying a confirmed `skill_update` or SkillOS-lite curation proposal, invoke `skill-builder` with these inputs:
   - **skill name or path**: the `target_skill` from the candidate (e.g., `task-runner` or `skills/task-runner/`)
   - **change request**: a natural-language description combining the `change_summary` and `evidence` so skill-builder can understand what to change and why
   - **target section hint**: the `section` field to help skill-builder locate the edit target
   For SkillOS-lite `insert`, use the proposed new `target_skill` name. For `deprecate`, ask `skill-builder` to add clear deprecation guidance rather than deleting files unless the user explicitly requests deletion.
   Do not hand-edit skill files from `insights` without `skill-builder`. Let `skill-builder` perform the edit, validate frontmatter, confirm references, and report the result.

## Candidate Types

- `project_note`: accepted project facts, commands, paths, conventions, and validation entry points.
- `workflow_rule`: VibeRig process improvements proven by accepted work.
- `skill_update`: proposed changes to `skills/*/SKILL.md`. Must include:
  - `target_skill`: directory name under `skills/` (e.g., `task-runner`)
  - `change_summary`: one-line description of what should change and why
  - `evidence`: the accepted work evidence that motivated this change
  - `section`: the SKILL.md section to modify (e.g., `## Workflow`, `## Validation`, `## Common Mistakes`)
  - `confidence`: `high` | `medium` | `low`
- `user_preference`: durable user preference, always requiring explicit confirmation.

## SkillOS-Lite Curation Proposals

Use `skillos-lite` after candidate generation when the accepted evidence may affect the reusable skill library.

Allowed operations:

- `insert`: propose a new skill for a reusable accepted workflow that does not fit an existing skill.
- `update`: propose a targeted change to an existing skill.
- `deprecate`: propose deprecation for duplicated, stale, misleading, or risky skill guidance. Prefer deprecation over deletion.
- `noop`: record that no skill-library change is justified.

Every non-`noop` curation proposal requires explicit user confirmation and must be applied through `skill-builder`.

## Output Contract

Prefer a Linear comment or user-facing summary for retrospectives. Format retrospectives using `references/report-template.md`. Ask `vb-linear` to write the retrospective comment back to the Linear issue. Ask `vb-linear` to write a Linear document only when the user explicitly wants a document-level retrospective or project learning note. Only write local files when the project already has an approved local learning location or the user asks for one.

Do not write proof packets into `.vibeRig/`.

For skill changes and confirmed SkillOS-lite curation proposals, invoke `skill-builder` and report its result: changed file paths, the trigger intent captured in the updated description, any resources added or omitted, validation performed, and remaining gaps. If skill-builder reports validation failures, surface them to the user as pending follow-up.

## Red Flags

- A learning candidate was applied without explicit user confirmation → only `project_note` auto-apply is allowed; all others require confirmation.
- A skill file was edited directly from `insights` without routing through `skill-builder` → stop; hand off to `skill-builder` for all `skill_update` and curation proposals.
- Evidence is sourced from a failed, abandoned, or unmerged attempt → stop; generate `noop` and explain the missing acceptance boundary.
- A retrospective was generated before checking the issue's accepted/done status → validate the acceptance gate first.

## Anti-Rationalization

| Rationalization | Reality |
|---|---|
| "The implementation worked well, I'll capture it as a workflow rule now" | A workflow rule requires accepted work. If the PR is not merged or the issue is not in a terminal done state, the evidence is not accepted. |
| "I'll hand-edit the skill file since the change is tiny" | Even tiny skill edits require `skill-builder` to validate frontmatter, confirm references, and ensure the description still passes trigger quality checks. |
| "The user confirmed the work, so I can skip asking about the learning candidate" | Accepting the work is not the same as confirming the learning candidate. Ask explicitly before applying workflow rules, skill updates, or user preferences. |

## Validation

```bash
# Confirm acceptance boundary exists before generating candidates
grep -q "Done\|Accepted\|Completed" <(echo "<Linear issue status>") && echo "ok" || echo "NOT ACCEPTED"
```

- [ ] Evidence is tied to accepted work or the user explicitly authorized retrospective generation.
- [ ] Each candidate has a type, evidence source, confidence, and risk.
- [ ] Durable project notes are high-confidence and low-risk.
- [ ] Workflow rules, skill updates, and user preferences were not applied without explicit user confirmation.
- [ ] Any skill file creation or modification was performed through `skill-builder`.
- [ ] Any SkillOS-lite `insert`, `update`, or `deprecate` proposal was explicitly confirmed and applied through `skill-builder`.
- [ ] Human-facing retrospectives, Linear comments, documents, and summaries use `output.language` when configured.
