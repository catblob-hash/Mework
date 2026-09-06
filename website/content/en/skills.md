# Skills

A **skill** is a folder with a `SKILL.md` file: packaged instructions for one kind of task — a deploy checklist, a review procedure, a repository's conventions — optionally with scripts and reference files next to it. The format is the same one Claude Code, Codex and the public skill registries use, so existing skills install unchanged.

## The `SKILL.md` format

```markdown
---
name: Commit helper
description: Prepare a conventional commit from the current diff
when_to_use: The user asks to commit, stage, or write a commit message
author: Example Org
version: 1.0.0
tags: [git, commits]
---

# Commit helper

1. Run `git status` and `git diff --staged`.
2. Group unrelated changes into separate commits.
3. Write a conventional commit message; run `scripts/check-message.sh` on it.
```

The frontmatter is flat `key: value` pairs (not full YAML). Mework reads exactly these keys and ignores others:

| Key | Used for |
|---|---|
| `name` | The skill's name in the catalog and the value the model passes to the `skill` tool. Falls back to the first heading, then the folder name. |
| `description` | Shown in the catalog and, together with `when_to_use`, is the **trigger** text the model sees. |
| `when_to_use` | Appended to the trigger as `description - when_to_use`. |
| `author`, `version`, `tags` | Catalog metadata only. Tags may be `[a, b]` or `a, b`. |

The **body** (everything after the frontmatter) is what the model reads. It is user-authored content and enters the model context verbatim, so write it as instructions to the model. Relative paths in it (`scripts/…`, `references/…`) resolve against the skill's directory, which the `skill` tool returns alongside the body.

Limits: a skill is at most 100 MiB and 2 000 entries; symlinks, absolute paths and `..` segments in archives are rejected; folder names are unique case-insensitively.

## Installing

Settings → **Skills** → **Add skill** offers three sources:

- **Search online** — skills.sh, claude-plugins.dev and clawhub.ai are searched by the host concurrently; the GitHub tab takes a direct link to a `SKILL.md` file in a repository. Installs use `git` (a shallow fetch of just the skill directory) or a ZIP download; `git` must be on your `PATH` for the git-backed sources.
- **Search this machine** — scans `~/.mework/skills`, `~/.claude/skills`, `~/.codex/skills`, `~/.config/skills` and the `.mework/skills` / `.claude/skills` folders of your workspaces, and lists what it finds for import.
- **Import locally** — a folder or a ZIP file.

Every source ends in the same installer: the skill is **copied** into `%APPDATA%\com.mework.app\skills\<folder>` and registered in the settings document. Copying rather than referencing is deliberate: the body is pasted into model context, and a folder outside the app could be rewritten between two turns by any other program. Editing the original afterwards does not change the installed copy — reinstall to update.

Each card in the Skills page has an **enabled** switch (global) and an uninstall action.

## Selecting skills for a conversation

Installation makes a skill available; it is used only when a conversation (or the preset it starts from) selects it in the **Skills** section of the settings drawer. Both must hold: the global switch on and the id selected. A selected skill that is disabled or missing shows as *selected, currently inactive* and contributes nothing.

## How skills reach the model

The **Skill delivery** switch (in the same drawer, shown once a skill is selected) chooses between two mechanisms:

- **Off (default) — pasted into the system prompt.** The body of every selected skill is appended to the system prompt, separated by `---` lines. The heading and the frontmatter are not included. The model reads the skills on every turn, and pays their tokens on every turn.
- **On — loaded on demand with the `skill` tool.** The system prompt does not contain the bodies. Instead the model gets a `skill` tool whose `name` parameter is an `enum` of the selected skills and whose description lists each skill's trigger text (`skill.tool_description` and `skill.listing_row` in the [prompt profile](prompt-profiles.html)). When the model calls it, the tool returns `Base directory for this skill: <path>` followed by the body (`skill.result`). Two selected skills with the same name are refused in this mode, because the enum could not tell them apart.

The `skill` tool is derived, not in the tool picker: it appears exactly when the switch is on and at least one selected skill resolved. Subagents inherit the parent conversation's selected skills.

## Verifying that a skill is active

- With delivery **off**, the next model reply will follow the skill's instructions; in the developer console of the model provider (or a mock provider) the body appears in the system prompt after the `---` separator.
- With delivery **on**, the timeline shows a `skill` tool call card whose result begins with the base directory line when the model loads it.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The skill is not in the drawer | It is not installed — the drawer lists installed skills only. Use Add skill. |
| Selected, currently inactive | The global switch is off, or the installed folder lost its `SKILL.md`. |
| Two skills share a name and on-demand loading fails | Rename one in its `SKILL.md` (`name:`) and reinstall, or select only one. |
| Online search fails for one registry | Registries are independent; the others still return results. GitHub direct links must end in `/SKILL.md`. |
| Online install fails immediately | `git` is not on `PATH`; see Settings → Environment dependencies. |
