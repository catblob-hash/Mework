import type {
  ConversationToolLock,
  DecisionParameterMode,
  DecisionParameterModes,
  RememberedDecisionForm,
  RememberedDecisionForms,
  RememberedToolFamilies
} from "../types";
import {
  DECISION_PARAMETER_TOOL_NAMES,
  DECISION_VARIANT_TOOLS,
  decisionVariantOf,
  MISS_SCORING_TOOL_NAMES,
  scoresMisses,
  takesDecisionParameters
} from "./taskTools";

/**
 * One tool's decision-model form: a mode, or `null` for "direct parameters
 * only". Mirrors Rust `Option<DecisionParameterMode>`.
 */
export type DecisionForm = DecisionParameterMode | null;

/**
 * The narrowest form whose schema accepts every call both arguments do.
 * `augment` accepts everything; the direct form and `replace` accept disjoint
 * calls, so together they make `augment`. Mirrors Rust
 * `DecisionParameterMode::join`.
 */
export function joinDecisionForms(left: DecisionForm, right: DecisionForm): DecisionForm {
  if (left === null && right === null) return null;
  if (left === "replace" && right === "replace") return "replace";
  return "augment";
}

/**
 * The form a tool's exposure has covered so far, or `undefined` when no run has
 * exposed the tool at all and every form is still open.
 */
function decisionFormFloor(lock: ConversationToolLock, tool: string): DecisionForm | undefined {
  if (!lock.tools.includes(tool)) return undefined;
  return lock.decisionParameterModes?.[tool] ?? null;
}

/**
 * Whether a tool may be put in `target` without narrowing what the transcript
 * holds: the target has to accept every call the floor does.
 */
export function decisionFormAllowed(floor: DecisionForm | undefined, target: DecisionForm): boolean {
  return floor === undefined || joinDecisionForms(floor, target) === target;
}

/** Keeps the entries this build understands, for decision-parameter tools only. */
export function normalizeDecisionParameterModes(value: unknown): DecisionParameterModes {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const modes: DecisionParameterModes = {};
  for (const [tool, mode] of Object.entries(value as Record<string, unknown>)) {
    if (takesDecisionParameters(tool) && (mode === "augment" || mode === "replace")) modes[tool] = mode;
  }
  return modes;
}

/** The modes of the decision-parameter tools among `tools`, in catalog order. */
export function decisionModesFor(
  modes: DecisionParameterModes | undefined,
  tools: readonly string[]
): DecisionParameterModes {
  const present = new Set(tools);
  const picked: DecisionParameterModes = {};
  for (const tool of DECISION_PARAMETER_TOOL_NAMES) {
    const mode = modes?.[tool];
    if (present.has(tool) && mode) picked[tool] = mode;
  }
  return picked;
}

/**
 * The floors after one exposure: each exposed decision-parameter tool's form
 * joined with what the lock already covered. Entries for tools the exposure
 * did not touch are kept as they are.
 */
export function mergeDecisionParameterModes(
  lock: ConversationToolLock,
  exposedTools: readonly string[],
  exposedModes: DecisionParameterModes | undefined
): DecisionParameterModes {
  const merged: DecisionParameterModes = { ...(lock.decisionParameterModes ?? {}) };
  for (const tool of exposedTools) {
    if (!takesDecisionParameters(tool)) continue;
    const exposed = exposedModes?.[tool] ?? null;
    const floor = decisionFormFloor(lock, tool);
    const form = floor === undefined ? exposed : joinDecisionForms(floor, exposed);
    if (form === null) delete merged[tool];
    else merged[tool] = form;
  }
  return merged;
}

/**
 * The already-exposed tools whose form `requested` would widen, each with the
 * form the next run would expose. A tool no run has exposed is absent: adding
 * it is already an addition by name.
 */
export function widenedDecisionParameterModes(
  lock: ConversationToolLock,
  requested: DecisionParameterModes | undefined
): DecisionParameterModes {
  const widened: DecisionParameterModes = {};
  for (const tool of DECISION_PARAMETER_TOOL_NAMES) {
    const floor = decisionFormFloor(lock, tool);
    if (floor === undefined) continue;
    const form = joinDecisionForms(floor, requested?.[tool] ?? null);
    if (form !== null && form !== floor) widened[tool] = form;
  }
  return widened;
}

/**
 * `requested` for the decision-parameter tools among `tools`, each widened to
 * cover what the lock says the transcript already holds — the same answer the
 * host computes for a run, so the settings never promise a narrower form than
 * the model will actually be shown.
 */
export function flooredDecisionParameterModes(
  lock: ConversationToolLock,
  requested: DecisionParameterModes | undefined,
  tools: readonly string[]
): DecisionParameterModes {
  const present = new Set(tools);
  const floored: DecisionParameterModes = {};
  for (const tool of DECISION_PARAMETER_TOOL_NAMES) {
    if (!present.has(tool)) continue;
    const floor = decisionFormFloor(lock, tool);
    const wanted = requested?.[tool] ?? null;
    const form = floor === undefined ? wanted : joinDecisionForms(floor, wanted);
    if (form !== null) floored[tool] = form;
  }
  return floored;
}

export function sameDecisionParameterModes(
  left: DecisionParameterModes | undefined,
  right: DecisionParameterModes | undefined
): boolean {
  const leftEntries = Object.entries(left ?? {}).filter(([, mode]) => mode);
  const rightEntries = Object.entries(right ?? {}).filter(([, mode]) => mode);
  return leftEntries.length === rightEntries.length
    && leftEntries.every(([tool, mode]) => right?.[tool] === mode);
}

/** Keeps the tool names this build can score misses for, deduplicated, in catalog order. */
export function normalizeDecisionMissScoring(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const named = new Set(value.filter((tool): tool is string => typeof tool === "string" && scoresMisses(tool)));
  return MISS_SCORING_TOOL_NAMES.filter((tool) => named.has(tool));
}

/**
 * The entries of `scoring` that can take effect: tools among `tools` that
 * `modes` gives a decision form, since a tool on its direct form never asks
 * the decision model and so never misses. Catalog order. The same answer the
 * host computes for a run.
 */
export function decisionMissScoringFor(
  scoring: readonly string[] | undefined,
  modes: DecisionParameterModes | undefined,
  tools: readonly string[]
): string[] {
  const present = new Set(tools);
  const wanted = new Set(scoring ?? []);
  return MISS_SCORING_TOOL_NAMES.filter((tool) => wanted.has(tool) && present.has(tool) && Boolean(modes?.[tool]));
}

export function sameDecisionMissScoring(
  left: readonly string[] | undefined,
  right: readonly string[] | undefined
): boolean {
  const leftSet = new Set(left ?? []);
  const rightSet = new Set(right ?? []);
  return leftSet.size === rightSet.size && [...leftSet].every((tool) => rightSet.has(tool));
}

/**
 * The tool-list fields one picker edit writes together: which tools are on,
 * the forms and miss scoring of the ones that are, and the choices remembered
 * for the ones that are off — a tool's form, and a whole family's rows.
 * `rememberedToolFamilies` is absent where the owner keeps none of its own.
 */
export interface DecisionToolSettings {
  enabledTools: string[];
  decisionParameterModes: DecisionParameterModes;
  decisionMissScoring: string[];
  rememberedDecisionForms: RememberedDecisionForms;
  rememberedToolFamilies?: RememberedToolFamilies;
}

/** Every row the settings draw with sub-options: a decision-parameter tool, or a base tool with a variant. */
const DECISION_ROWS: readonly string[] = [
  ...DECISION_PARAMETER_TOOL_NAMES,
  ...Object.keys(DECISION_VARIANT_TOOLS)
];

function hasDecisionForm(row: string): boolean {
  return takesDecisionParameters(row) || decisionVariantOf(row) !== null;
}

/** Keeps the entries this build understands: rows with a decision form, a known form, and miss scoring only where it can apply. */
export function normalizeRememberedDecisionForms(value: unknown): RememberedDecisionForms {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const remembered: RememberedDecisionForms = {};
  for (const [row, entry] of Object.entries(value as Record<string, unknown>)) {
    if (!hasDecisionForm(row) || !entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const { form, missScoring } = entry as Record<string, unknown>;
    if (form !== "augment" && form !== "replace") continue;
    remembered[row] = missScoring === true && scoresMisses(row) ? { form, missScoring: true } : { form };
  }
  return remembered;
}

/** The tool families a conversation can remember rows for. Mirrors `ToolFamilyId`. */
const TOOL_FAMILY_IDS: ReadonlySet<string> = new Set(["files", "shell", "preview"]);

/** Keeps the families this build has, each with its row names once and bounded. */
export function normalizeRememberedToolFamilies(value: unknown): RememberedToolFamilies {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const remembered: RememberedToolFamilies = {};
  for (const [family, rows] of Object.entries(value as Record<string, unknown>)) {
    if (!TOOL_FAMILY_IDS.has(family) || !Array.isArray(rows)) continue;
    const names = [...new Set(rows.filter((row): row is string => (
      typeof row === "string" && row.length > 0 && row.length <= 128
    )))].slice(0, 64);
    if (names.length) remembered[family] = names;
  }
  return remembered;
}

/** A tool-backed row's form, read off which of its two tools are on. */
function variantForm(baseOn: boolean, variantOn: boolean): DecisionForm {
  return baseOn && variantOn ? "augment" : variantOn ? "replace" : null;
}

/**
 * `settings` with `removed` switched off. Every row that goes from on to off
 * takes its live form and miss scoring into `rememberedDecisionForms`, so
 * switching it back on returns to them; a row that was on its direct form
 * leaves nothing to remember and forgets whatever an older switch-off kept.
 */
export function switchToolsOff(
  settings: DecisionToolSettings,
  removed: ReadonlySet<string>
): DecisionToolSettings {
  const before = new Set(settings.enabledTools);
  const enabledTools = settings.enabledTools.filter((tool) => !removed.has(tool));
  const after = new Set(enabledTools);
  const decisionParameterModes = { ...settings.decisionParameterModes };
  const rememberedDecisionForms = { ...settings.rememberedDecisionForms };
  let decisionMissScoring = [...settings.decisionMissScoring];
  for (const row of DECISION_ROWS) {
    const variant = decisionVariantOf(row);
    const names = variant ? [row, variant] : [row];
    if (!names.some((name) => before.has(name)) || names.some((name) => after.has(name))) continue;
    const form = variant
      ? variantForm(before.has(row), before.has(variant))
      : decisionParameterModes[row] ?? null;
    if (form === null) {
      delete rememberedDecisionForms[row];
    } else {
      const remembered: RememberedDecisionForm = { form };
      if (scoresMisses(row) && decisionMissScoring.includes(row)) remembered.missScoring = true;
      rememberedDecisionForms[row] = remembered;
    }
    delete decisionParameterModes[row];
    decisionMissScoring = decisionMissScoring.filter((tool) => tool !== row);
  }
  return { ...settings, enabledTools, decisionParameterModes, decisionMissScoring, rememberedDecisionForms };
}

/**
 * `settings` with each of `rows` switched on, a row with a remembered choice
 * coming back to it. `available` is the catalog the owner can offer: a
 * remembered variant that is not in it comes back as its base tool alone. A
 * row that is already on keeps the form it has.
 */
export function switchToolsOn(
  settings: DecisionToolSettings,
  rows: readonly string[],
  available: ReadonlySet<string>
): DecisionToolSettings {
  const enabledTools = [...settings.enabledTools];
  const decisionParameterModes = { ...settings.decisionParameterModes };
  const rememberedDecisionForms = { ...settings.rememberedDecisionForms };
  let decisionMissScoring = [...settings.decisionMissScoring];
  for (const row of rows) {
    const variant = decisionVariantOf(row);
    const alreadyOn = enabledTools.includes(row) || (variant !== null && enabledTools.includes(variant));
    const remembered = alreadyOn ? undefined : rememberedDecisionForms[row];
    if (!remembered) {
      if (!alreadyOn) enabledTools.push(row);
      continue;
    }
    delete rememberedDecisionForms[row];
    if (variant) {
      const variantAvailable = available.has(variant);
      if (remembered.form === "augment" || !variantAvailable) enabledTools.push(row);
      if (variantAvailable) enabledTools.push(variant);
      continue;
    }
    enabledTools.push(row);
    if (takesDecisionParameters(row)) decisionParameterModes[row] = remembered.form;
    decisionMissScoring = decisionMissScoring.filter((tool) => tool !== row);
    if (remembered.missScoring && scoresMisses(row)) decisionMissScoring.push(row);
  }
  return {
    ...settings,
    enabledTools: Array.from(new Set(enabledTools)),
    decisionParameterModes,
    decisionMissScoring,
    rememberedDecisionForms
  };
}
