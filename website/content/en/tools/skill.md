The `skill` tool loads one of this conversation's selected skills at the moment it is needed. The model reaches for it when the task in front of it is one a skill covers — a deploy procedure, a review checklist, a repository's own conventions — and gets that skill's instructions back together with the folder they live in, so relative references inside the body resolve. It exists only while **Load skills on demand** is on; with the switch off the bodies are already in the system prompt and there is nothing to load.

## Approval

No security level asks for this call. The host read the skill bodies off disk before the turn started, so the call is a lookup in host memory rather than a file, shell or network action. It is scheduled only by the model run loop and cannot be executed or approved by hand.

## Behavior and limits

The tool is present only when at least one selected skill resolved. The name it takes is the skill's **directory** name, not the `name` in its frontmatter, and the schema names no skill at all — the available names and their triggers are listed in the conversation's context instead. A name that is not among this conversation's skills fails, listing the ones that are. Two selected skills whose directories share a name are refused before the run starts. `SKILL.md` is read with a 256 KiB limit; a selected skill whose body cannot be read fails the run with the reason, while one whose folder has disappeared is skipped. The result is the skill's base directory followed by its body, verbatim. Subagents inherit the parent conversation's resolved skills and may load them.

## Related

- [Skills](../skills.html) — the `SKILL.md` format, where skills are discovered, and both delivery modes.
- [tool_search](tool_search.html) — the same on-demand shape, for MCP tool schemas.
- [Prompt profiles](../prompt-profiles.html) — the `skill.*` keys carry the wording.
