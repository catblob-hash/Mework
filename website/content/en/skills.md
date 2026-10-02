# Skills

A **skill** is a folder with a `SKILL.md` file: packaged instructions for one kind of task — a deploy checklist, a review procedure, a repository's conventions — optionally with scripts and reference files next to it. The format is the same one Claude Code, Codex and the public skill registries use, so an existing skill works as it is.

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

The frontmatter is flat `key: value` pairs (not full YAML). Mework reads exactly these keys:

| Key | Used for |
|---|---|
| `name` | The skill's label in the catalog. Falls back to the first heading, then the directory name. The model addresses the skill by its **directory name**, not by this. |
| `description` | Shown in the catalog and, together with `when_to_use`, is the **trigger** text the model sees. Falls back to the body's first non-empty line that is not a heading. |
| `when_to_use` | Appended to the trigger as `description - when_to_use`; when neither the key nor that fallback yields any description, it is the trigger on its own. |
| `author`, `version`, `tags` | Parsed and then dropped: no page shows them and they never reach the model. Tags may be `[a, b]` or `a, b`. |

Every other Claude Code frontmatter key — `allowed-tools`, `disable-model-invocation`, `user-invocable`, `model`, `context`, and the rest — is **ignored**. Exposure of the `skill` tool is a conversation setting here, not a frontmatter key.

The **body** (everything after the frontmatter) is what the model reads. It is user-authored content and enters the model context verbatim, so write it as instructions to the model. Relative paths in it (`scripts/…`, `references/…`) are relative to the skill's directory: the `skill` tool returns that directory alongside the body, while a body pasted into the system prompt arrives without it.

## Where skills live

Skills are **not** installed, imported or copied. Mework scans two directories and lists what it finds:

```text
~/.mework/skills/<dir>/SKILL.md              user scope — every workspace
<workspace>/.mework/skills/<dir>/SKILL.md    workspace scope — that workspace only
```

The folder on disk **is** the skill. Edit `SKILL.md` or a script beside it and the change is in effect at the next run; delete the folder and the skill is gone. Discovery is not recursive: a directory counts only when it is a **direct child** of one of the two `skills/` folders, is not a symlink, and holds a regular file named exactly `SKILL.md`. A symlinked directory or manifest is ignored, so a link cannot hand what enters the model context to its target's owner. A directory name may not contain `(`, `)`, `,` or control characters, may not have leading or trailing whitespace, and may not be empty — the same rule Claude Code applies to slash commands, and here it also keeps the name the model addresses a skill by unambiguous. `SKILL.md` may be up to 256 KiB.

The skill's identity is its **directory name**: that is the value the model passes to the `skill` tool, as with `/<name>` in Claude Code. The frontmatter `name` and `description` are only the label and trigger the catalog shows.

## The built-in Mework SDK skill

One skill ships with the app: **Mework SDK** (`mework-sdk`), which teaches the model how to configure Mework itself — writing skills, `mcp.json`, `hooks.json`, `lsp.json`, prompt profiles, `launch.json`, `MEWORK.md` and rules, and pointing you at the right place in the app for the settings that are not files. It is compiled into the app rather than written to disk, so it updates with each version of Mework, cannot be edited, and has no delete button; it heads the **Skills** list and appears in every conversation's list, whatever its workspace.

Like every skill it does nothing until a conversation selects it: no preset selects it, the built-in **mework** preset included. Tick it in a conversation when you want the model to set up or troubleshoot Mework for you. Its id, `skill_builtin_mework_sdk`, stays the same across versions, so a preset of your own that selects it keeps it through updates. With on-demand loading it is listed as `mework-sdk`, and the `skill` tool returns it with no base directory, since it has no files.

Each discovered row in the conversation drawer's **Skills** page has a delete button that removes the skill's folder, `scripts/` and `references/` included, after a second click confirms; a skill the conversation's last request carried is drawn orange while the model's cache is warm (see [the tool lock](tools.html#lock)): it can still be turned off, after the cache warning, but it has no delete button. The page toolbar also has a **Rescan** button and **open folder** buttons: the global `~/.mework/skills`, plus this workspace's `.mework/skills` when the conversation has a workspace. They create the directory when it is not there yet and reveal it in your file manager. Rescan also runs at startup, when the settings pane opens, when a workspace is added, and after every delete; every run rescans for itself, so a run uses what is on disk at that moment.

## Selecting skills for a conversation

Discovery makes a skill available; it is used only when a conversation (or the preset it starts from) selects it in the **Skills** page. A conversation can select from the global `~/.mework/skills` plus **its own workspace's** `.mework/skills`; another workspace's skills are not offered. A preset may keep any id.

A selected id whose folder has disappeared — renamed or deleted — stays selected and shows as a **dangling** row. It is **skipped** at run time, so a deleted folder never bricks a conversation; uncheck the row to remove it. A skill the scan does find but cannot read — its `SKILL.md` is over the size limit or is not UTF-8 — is shown unavailable with the reason, and a run that selects it **fails** until you fix it or uncheck it.

## How skills reach the model

The **Load skills on demand** switch (under the list, always shown) chooses between two mechanisms:

- **Off, the switch reading *In the prompt* — pasted into the system prompt.** The body of every selected skill is appended to the system prompt, separated by `---` lines. The frontmatter is stripped; a heading the body itself opens with is kept. The model reads the skills on every turn, and pays their tokens on every turn.
- **On, *On demand* — loaded with the [`skill` tool](tools/skill.html).** The system prompt does not contain the bodies. It carries the catalog instead: an `Available skills:` heading and one `- <name>: <trigger>` line per skill that has trigger text (`skill.listing_heading` and `skill.listing_row` in the [prompt profile](prompt-profiles.html)). The tool's schema names no skill — it takes the folder `name` as a string, so it is the same object in every conversation — and calling it returns `Base directory for this skill: <path>` followed by the body (`skill.result`). Two selected skills whose directories share a name are refused in this mode, because one name could not tell them apart.

A new conversation takes the switch from the preset it starts from; the built-in **mework** preset starts it on.

Either way, the skills a conversation **starts** with are the ones its system prompt is built from, and that prompt does not change afterwards. Selecting another skill later delivers it at that point in the transcript as a host message through the [`box` tool](tools/box.html) — the body with delivery off, the trigger line with delivery on (`system.skill_added_body` / `system.skill_added_trigger`, under the summary `host_notice.skill_added_summary`) — so the cached prompt ahead of it stays as it was. Taking a skill off, or moving the delivery switch, does rewrite that prompt, which is why both are drawn orange while the cache is warm; neither is refused.

The `skill` tool is derived, not in the tool picker: it appears exactly when the switch is on and at least one selected skill resolved. Subagents inherit the parent conversation's selected skills.

## Verifying that a skill is active

- With delivery **off**, the next model reply will follow the skill's instructions; the **History** tab (More options → History) shows the body inside the system prompt, after the `---` separator.
- With delivery **on**, the timeline shows a `skill` tool call card whose result begins with the base directory line when the model loads it.

## Troubleshooting

| Symptom | Cause |
|---|---|
| Mework SDK has no delete button | It is built into the app. Uncheck it to stop using it. |
| The skill is not in the drawer | Its folder is not a direct child of `~/.mework/skills` or of this workspace's `.mework/skills`, or it has no `SKILL.md`, or it is a symlink, or its name contains `(`, `)` or `,`, or it has leading or trailing whitespace. |
| Shown as unavailable | Mework found the folder but cannot read `SKILL.md`: over 256 KiB, not UTF-8, or unreadable. A run that selects it fails with that reason. |
| Shown as *Dangling* | The selected id's folder has been renamed or deleted. It is ignored at run time; uncheck the row to remove it. |
| Two skills share a directory name and on-demand loading fails | Rename one directory, or select only one. |
| Two skills share a display name | Harmless — the model selects by directory name, not by the label. |
| A change to a skill is not visible | The catalog is a snapshot. Press **Rescan** on the page; startup, opening the settings pane and deleting an entry scan too. |
