---
name: create-skill
description: Author a new reusable skill (SKILL.md). Use when the user wants to create, add, or define a new skill/workflow so it becomes discoverable by search_skills and list_skills.
---
# create-skill

Create a new skill so future runs can discover and reuse a workflow.

A skill is a directory containing a single `SKILL.md` file:

```
<skills-dir>/<skill-name>/SKILL.md
```

## Format

`SKILL.md` is YAML frontmatter followed by a markdown body:

```
---
name: my-skill
description: One or two sentences. Say what it does AND when to use it, because this text is what search_skills matches against.
---
# my-skill

Step-by-step instructions the agent should follow when this skill is selected.
```

Constraints (enforced by the loader; a file that violates them is silently ignored):
- `name`: lowercase letters, digits, `-`, `/` only (regex `^[a-z0-9\-/]+$`). It must match the directory name.
- `description`: <= 1024 chars. Front-load "what + when to use" so retrieval ranks it well.
- Whole file <= 65536 bytes.

## Steps

1. Pick a short, kebab-case `name` and confirm it is not already taken (use `list_skills`).
2. Decide where it should live:
   - Project-local (only this workspace): `.deepseek-science/skills/<name>/SKILL.md` inside the workspace.
   - Global (all projects): tell the user to add it under their data dir `~/.deepseek-science/skills/<name>/SKILL.md`, or add a custom skills directory in Settings → Skills.
3. Write the `SKILL.md` with the frontmatter + body using the `write_file` tool. Keep the body concrete: numbered steps, which tools to call, and the expected artifacts.
4. Ask the user to confirm the content, then verify with `list_skills` / `search_skills` that it is discoverable. (Newly written project skills are picked up on the next run; global/custom-dir skills after the catalog is rebuilt or the app restarts.)

## Tips

- Write the description for the searcher, not the human: include trigger words a user might phrase their request with.
- Prefer one focused skill over a giant catch-all.
- Reference concrete tool names (`web_search`, `write_file`, `compile_pdf`, `ask_user`, …) in the body so the workflow is executable.
