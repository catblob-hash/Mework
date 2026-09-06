import type {
  AgentDefinition,
  AgentModelSelection,
  ApiProvider,
  ReasoningEffort,
  SearchProviderSelection
} from "../types";

export const MAX_AGENT_TYPE_CHARS = 64;
/** Mirrors the Rust `MAX_AGENT_DEFINITION_TOOL_NAMES` / `_TOOL_NAME_CHARS`. */
export const MAX_AGENT_DEFINITION_TOOL_NAMES = 512;
export const MAX_AGENT_DEFINITION_TOOL_NAME_CHARS = 256;
/** An allowlist entry meaning "keep everything"; mirrors the Rust wildcard. */
export const AGENT_TOOL_WILDCARD = "*";

export type AgentTypeSlugError =
  | "required"
  | "too_long"
  | "first_character"
  | "characters";

/**
 * Whether a role's bound model resolves against the current providers.
 *
 * Mirrors the Rust `agent_definition_model_is_available`, and the failure cases
 * of `enabled_provider_model` it delegates to: provider missing, provider
 * disabled, model missing under that provider. Presence in the provider's model
 * list is the whole of a model's availability — a model that is listed is usable.
 * `explicit` records the exact `(providerId, modelId)` PAIR because two providers
 * may both carry a model called `gpt-4o` and they are not the same model —
 * matching on the bare ID would silently rebind a role to some other provider's
 * model.
 *
 * `inherit` always resolves: it rides the caller's own provider and model.
 * `unavailable` never does — it exists precisely to record a binding that has
 * already been found dead, so it is not re-checked against anything.
 *
 * This matters beyond the editor's own validation. The host hides a role whose
 * model does not resolve from the listing it sends the model, so such a role is
 * uncallable — and the settings list has to say so, or a user reads a normal
 * row as "this works".
 */
export function agentDefinitionModelIsAvailable(
  selection: AgentModelSelection,
  providers: readonly ApiProvider[]
): boolean {
  if (selection.kind === "inherit") return true;
  if (selection.kind === "unavailable") return false;
  const provider = providers.find((candidate) => candidate.id === selection.providerId);
  if (!provider || !provider.enabled) return false;
  return provider.models.some((candidate) => candidate.id === selection.modelId);
}

/**
 * Whether the conversation has any available role.
 *
 * This is the renderer's approximation of `api::available_agent_type_names`:
 * enabled, not deleted, and bound to a resolvable model. It cannot see workspace
 * applicability or same-name precedence, so it controls only whether the
 * no-role-subagent toggle is shown. This is safe because the host falls back
 * whenever its final set is empty; an extra visible toggle cannot remove tools.
 */
export function hasUsableAgentDefinition(
  definitions: readonly AgentDefinition[],
  providers: readonly ApiProvider[]
): boolean {
  return definitions.some(
    (definition) =>
      definition.enabled
      && !definition.deleted
      && agentDefinitionModelIsAvailable(definition.modelSelection, providers)
  );
}

export interface UserAgentDefinitionDraft {
  name: string;
  /** Free text the host renders into the model-facing tool description. Not
   * capability-bearing, so it stays out of `sameUserAgentConfiguration`. */
  description: string;
  modelSelection: AgentModelSelection;
  /** Execution overrides. `null` / `[]` mean "no override". `tools` selects out
   * of the trusted catalogue rather than intersecting the conversation's own
   * set, so a role can grant a tool its caller does not hold; `disallowedTools`
   * subtracts. */
  effort: ReasoningEffort | null;
  tools: string[] | null;
  disallowedTools: string[];
  /** This role's `web_search` backend; `null` follows the conversation. */
  searchProvider: SearchProviderSelection | null;
}

/**
 * Whether this UI may edit the definition — and therefore whether it appears as
 * a row at all.
 *
 * A conversation's list legitimately carries project/plugin/managed roles the
 * host injected, and those are read-only here. Every surface that COUNTS roles
 * has to use this same predicate as the surface that LISTS them: a summary
 * derived from the raw array length would announce roles the list then declines
 * to draw, which reads as a rendering bug rather than as "some of these are not
 * yours". Tombstones are filtered upstream by `normalizeAgentDefinitions`, but
 * `deleted` is checked here too so this stays total on its own.
 */
export function editableUserAgentDefinition(definition: AgentDefinition): boolean {
  return (
    !definition.deleted
    && definition.source === "user"
    && definition.sourceKey === ""
  );
}

export function validateAgentTypeSlug(value: string): AgentTypeSlugError | null {
  const characters = Array.from(value);
  if (characters.length === 0) return "required";
  if (characters.length > MAX_AGENT_TYPE_CHARS) return "too_long";
  if (!/^[a-z]$/.test(characters[0])) return "first_character";
  if (!characters.every((character) => /^[a-z0-9_-]$/.test(character))) {
    return "characters";
  }
  return null;
}

/**
 * Whether a persisted value is a usable tool-name list.
 *
 * Deliberately strict and total: the caller must be able to treat "not this
 * shape" as a reason to reject the whole definition rather than to substitute a
 * default, because for a capability-bearing list every default is a policy
 * choice and the permissive one is the wrong one to make silently. Mirrors the
 * Rust `validate_agent_definitions` checks entry for entry.
 */
export function isAgentToolNameList(value: unknown): value is string[] {
  return Array.isArray(value)
    && value.length <= MAX_AGENT_DEFINITION_TOOL_NAMES
    && value.every((entry) =>
      typeof entry === "string"
      && entry.length > 0
      && entry.trim() === entry
      && Array.from(entry).length <= MAX_AGENT_DEFINITION_TOOL_NAME_CHARS
      && !/[\u0000-\u001f\u007f]/.test(entry));
}

/** Sorted and de-duplicated, matching the host's canonicalization so a reorder
 * in the editor is not mistaken for a configuration change. */
export function canonicalAgentToolNames(names: readonly string[]): string[] {
  return [...new Set(names)].sort();
}

export function userAgentDefinitionDraft(
  definition: AgentDefinition
): UserAgentDefinitionDraft {
  return {
    name: definition.name,
    // `?? ""` for the same reason as the three below: a definition persisted
    // without this key must open on "the owner wrote nothing" rather than
    // `undefined`.
    description: definition.description ?? "",
    modelSelection: { ...definition.modelSelection },
    // `??` on all three: a definition persisted before these keys existed has
    // none of them, and the draft must open on "no override" rather than on
    // `undefined`.
    effort: definition.effort ?? null,
    tools: definition.tools === null || definition.tools === undefined
      ? null
      : [...definition.tools],
    disallowedTools: [...(definition.disallowedTools ?? [])],
    // `?? null` for the same reason: a definition persisted without this key
    // follows the conversation, so the draft must do the same.
    searchProvider: definition.searchProvider
      ? { ...definition.searchProvider }
      : null
  };
}

export function sameAgentModelSelection(
  left: AgentModelSelection,
  right: AgentModelSelection
): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind !== "explicit" || right.kind !== "explicit") return true;
  return (
    left.providerId === right.providerId
    && left.modelId === right.modelId
  );
}

export function sameUserAgentConfiguration(
  definition: AgentDefinition,
  draft: UserAgentDefinitionDraft
): boolean {
  // `description` is deliberately absent. It is prose the model reads, never a
  // capability the child holds, and this predicate is what decides whether the
  // revision advances. Including it would mint a bump the host then rewrites
  // away (its own `same_user_agent_configuration` compares three other fields
  // entirely) — and were the host taught the same rule, rewording a role would
  // revoke every child already bound to it.
  return (
    sameAgentModelSelection(definition.modelSelection, draft.modelSelection)
    // The execution overrides: an override edit that left the revision alone
    // would be a capability change the identity ledger never records. Note the
    // host's own `same_user_agent_configuration` compares only
    // enabled/modelSelection/memory and REWRITES whatever revision arrives
    // here, so this term is the renderer's own stricter reading rather than a
    // mirror of it.
    && (definition.effort ?? null) === draft.effort
    && sameAgentToolNames(definition.tools ?? null, draft.tools)
    && sameAgentToolNames(definition.disallowedTools ?? [], draft.disallowedTools)
    && sameSearchProviderSelection(
      definition.searchProvider ?? null,
      draft.searchProvider
    )
  );
}

function sameSearchProviderSelection(
  left: SearchProviderSelection | null,
  right: SearchProviderSelection | null
): boolean {
  if (left === null || right === null) return left === right;
  if (left.kind !== right.kind) return false;
  if (left.kind !== "explicit" || right.kind !== "explicit") return true;
  return left.providerKind === right.providerKind;
}

function sameAgentToolNames(
  left: readonly string[] | null,
  right: readonly string[] | null
): boolean {
  if (left === null || right === null) return left === right;
  return left.length === right.length && left.every((name, index) => name === right[index]);
}

/**
 * Builds the only definition shape this UI is allowed to author.
 *
 * The trusted host owns source/sourceKey/revision/memoryEpoch. The renderer
 * carries the current epoch across ordinary edits and uses provisional epoch 1
 * for a new identity; Rust remains the only authority that may accept or
 * advance it. Renaming is intentionally a delete plus create (revision 1), so
 * an old agent-memory partition is never silently moved to another identity.
 */
export function buildUserAgentDefinition(
  draft: UserAgentDefinitionDraft,
  previous: AgentDefinition | null = null
): AgentDefinition {
  // Canonicalize BEFORE the comparison, matching the order the host uses in
  // `canonicalize_renderer_agent_definitions`. Comparing the raw draft would let
  // a drag-reorder or a duplicated entry read as a configuration change, bump
  // the revision, and invalidate every live binding for this definition.
  const canonicalDraft: UserAgentDefinitionDraft = {
    ...draft,
    tools: draft.tools === null ? null : canonicalAgentToolNames(draft.tools),
    disallowedTools: canonicalAgentToolNames(draft.disallowedTools)
  };
  const sameIdentity = previous !== null
    && previous.source === "user"
    && previous.sourceKey === ""
    && previous.name === canonicalDraft.name;
  const revision = !sameIdentity
    ? 1
    : sameUserAgentConfiguration(previous, canonicalDraft)
      ? previous.revision
      : previous.revision + 1;

  if (!Number.isSafeInteger(revision) || revision < 1) {
    throw new Error("agent_definition_revision_exhausted");
  }
  const memoryEpoch = sameIdentity ? previous.memoryEpoch : 1;
  if (!Number.isSafeInteger(memoryEpoch) || memoryEpoch < 1) {
    throw new Error("agent_definition_memory_epoch_invalid");
  }

  return {
    // A user-authored role lives inside a conversation preset, so the preset is
    // already its on/off switch and the renderer always writes `true`. The field
    // stays because a trusted project/plugin/managed source may still ship a
    // disabled definition, and that one keeps shadowing a user role by name.
    enabled: true,
    deleted: false,
    name: canonicalDraft.name,
    description: canonicalDraft.description,
    source: "user",
    sourceKey: "",
    revision,
    memoryEpoch,
    modelSelection: { ...canonicalDraft.modelSelection },
    // Host-owned and part of the frozen binding projection; a renderer-authored
    // role never asks for a memory partition of its own.
    memory: "none",
    effort: canonicalDraft.effort,
    tools: canonicalDraft.tools,
    disallowedTools: canonicalDraft.disallowedTools,
    searchProvider: canonicalDraft.searchProvider
      ? { ...canonicalDraft.searchProvider }
      : null
  };
}
