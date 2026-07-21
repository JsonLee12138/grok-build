<!-- code-review-graph MCP tools -->
## MCP Tools: code-review-graph

**IMPORTANT: This project has a knowledge graph. ALWAYS use the
code-review-graph MCP tools BEFORE using Grep/Glob/Read to explore
the codebase.** The graph is faster, cheaper (fewer tokens), and gives
you structural context (callers, dependents, test coverage) that file
scanning cannot.

### When to use graph tools FIRST

- **Exploring code**: `semantic_search_nodes` or `query_graph` instead of Grep
- **Understanding impact**: `get_impact_radius` instead of manually tracing imports
- **Code review**: `detect_changes` + `get_review_context` instead of reading entire files
- **Finding relationships**: `query_graph` with callers_of/callees_of/imports_of/tests_for
- **Architecture questions**: `get_architecture_overview` + `list_communities`

Fall back to Grep/Glob/Read **only** when the graph doesn't cover what you need.

### Key Tools

| Tool | Use when |
|------|----------|
| `detect_changes` | Reviewing code changes — gives risk-scored analysis |
| `get_review_context` | Need source snippets for review — token-efficient |
| `get_impact_radius` | Understanding blast radius of a change |
| `get_affected_flows` | Finding which execution paths are impacted |
| `query_graph` | Tracing callers, callees, imports, tests, dependencies |
| `semantic_search_nodes` | Finding functions/classes by name or keyword |
| `get_architecture_overview` | Understanding high-level codebase structure |
| `refactor_tool` | Planning renames, finding dead code |

### Workflow

1. The graph auto-updates on file changes (via hooks).
2. Use `detect_changes` for code review.
3. Use `get_affected_flows` to understand impact.
4. Use `query_graph` pattern="tests_for" to check coverage.

<!-- inject:viberig:start -->
## VibeRig Output Language

- Read `.vibeRig/project.yaml` before creating or updating VibeRig human-facing records.
- Use `.vibeRig/project.yaml` `output.language` for VibeRig issue titles, issue descriptions, comments, requirement documents, validation notes, proof packets, human acceptance records, retrospectives, and final summaries.
- If `output.language` is missing, infer the language from the user's current working language, state the fallback, and recommend reconciling `.vibeRig/project.yaml` through `vb-init`.
- Do not translate stable IDs, file paths, commands, branch names, PR URLs, commit hashes, Linear keys, acceptance IDs, schema field names, code symbols, or existing external labels/status names.

## Output

Choose one primary format per reply. Up to two lines of context (purpose, assumption, or constraint) are allowed; do not append full prose or extra code blocks.

Evaluate in order, stop at first match:

**1. Structure / relationships / flow / state → Mermaid diagram**

| Trigger | Diagram type |
|---|---|
| DB table relationships, schema design | `erDiagram` |
| API call chains, auth, microservice interactions | `sequenceDiagram` |
| Business flows, CI/CD, ETL, retry/fallback | `flowchart` |
| State machines, lifecycle | `stateDiagram-v2` |
| Domain models, class inheritance, module deps | `classDiagram` |
| System/deployment architecture | `flowchart` / `architecture-beta` |
| Branch strategy, release flows | `gitGraph` |
| Requirement breakdown, brainstorming | `mindmap` |
| Technology selection, priority matrix | `quadrantChart` |

When also generating a code file: ① diagram in chat first, ② code to file. No code blocks in chat.

**2. Multi-dimensional comparison → Table**

**3. Ordered steps / task progress → Checklist**

**4. Reasoning / tradeoffs / explanation → 3W1H**

| **What** | Conclusion first |
|---|---|
| **Why** | Rationale, tradeoff basis |
| **How** | How to implement |
| **When** | Applicable boundary |

**5. Fallback → One sentence**
<!-- inject:viberig:end -->
