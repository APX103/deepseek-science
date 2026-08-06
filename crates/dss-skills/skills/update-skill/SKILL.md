---
name: update-skill
description: Edit or delete an existing skill (SKILL.md). Use when the user wants to update, rename, fix, improve, disable, or remove/delete a skill.
---
# update-skill

Modify or remove an existing skill. A skill is a directory with a single `SKILL.md`:

```
<skills-dir>/<skill-name>/SKILL.md
```

## Locate the skill first

1. Run `list_skills` (or `search_skills`) to find the skill and note its `source`:
   - `builtin` — shipped inside the app binary; cannot be edited on disk. To change behavior, create an overriding skill with the SAME `name` in a higher-priority source (global/project/custom); the later source wins.
   - `global` — `~/.deepseek-science/skills/<name>/SKILL.md`.
   - `project` — `<workspace>/.deepseek-science/skills/<name>/SKILL.md`.
   - `claude` / `codex` / `cursor` / `custom` — external directories enabled in Settings → Skills; edit the file where it lives.
2. Read the current `SKILL.md` before changing it.

## Update

1. Edit the frontmatter and/or body. Keep the constraints valid: `name` matches `^[a-z0-9\-/]+$` and the directory name, `description` <= 1024 chars, file <= 65536 bytes.
2. Write the file back with `write_file` (for project-local skills) or instruct the user for skills outside the workspace sandbox.
3. Renaming a skill = create the new `<new-name>/SKILL.md` and delete the old directory (see below), then update the `name` in the frontmatter to match.

## Delete

- Project-local skill: delete the file with the delete/remove file tool, i.e. remove `<workspace>/.deepseek-science/skills/<name>/SKILL.md` (and its now-empty directory).
- Skills outside the workspace (global/custom/external): guide the user to delete the `<name>` directory on disk, since these live outside the sandbox.
- Temporarily disable instead of deleting: in Settings → Skills, turn the skill off. It stays on disk and listed, but is hidden from `search_skills`/`list_skills`/`skill` until re-enabled.

## Verify

After any change, run `list_skills` / `search_skills` to confirm the result. Project skills take effect on the next run; global/custom/external changes apply after the catalog is rebuilt (saving Settings) or the app restarts.
