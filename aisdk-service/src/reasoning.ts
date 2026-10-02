//! Reasoning levels per family and model.
//!
//! The host sends one of five levels (`low` `medium` `high` `xhigh` `max`; the
//! composer calls `xhigh` "extra") and never names a model. Each upstream takes
//! a different subset, and the AI SDK cannot be left to sort it out:
//!
//! - its shared `reasoning` option has no `max`, and every provider that maps
//!   levels drops one it does not know with only a warning (`@ai-sdk/provider-utils`
//!   `mapReasoningToProviderEffort`), so `max` would silently mean no effort;
//! - an unsupported level is a hard `400` on OpenAI, Bedrock and Anthropic;
//! - some of its mappings raise the cost asked for (`xhigh` becomes `max` on the
//!   Claude 4.6 models and on every Bedrock model).
//!
//! So the rule here: send the highest level the model takes that is not above
//! the one asked for — the lowest it takes when all of them are above — and
//! never `none` or `minimal`, since there is no "off". Where the SDK's own
//! mapping already does that, the level goes through its shared `reasoning`
//! option; where it cannot, through the family's provider options.
//!
//! Model classes are read from ids, mirroring the SDK's own tables where it has
//! them (`@ai-sdk/anthropic` `getModelCapabilities`). An id this cannot place gets
//! the conservative ladder for its family. Chat Completions endpoints cannot be
//! known at all, so `chat-dialect.ts` retries a rejected `reasoning_effort` with
//! a value the endpoint names as supported.

import type { ReasoningLevel, StepRequest } from "./protocol.js";

/** The levels, lowest first. */
const LADDER: readonly ReasoningLevel[] = ["low", "medium", "high", "xhigh", "max"];
const UP_TO_HIGH: readonly ReasoningLevel[] = ["low", "medium", "high"];
const UP_TO_XHIGH: readonly ReasoningLevel[] = ["low", "medium", "high", "xhigh"];

/** The AI SDK's shared levels this module sends; `max` has no shared name. */
type SharedLevel = Exclude<ReasoningLevel, "max">;
type ProviderOptions = Record<string, Record<string, unknown>>;

export interface ReasoningPlan {
  /** The AI SDK `reasoning` option, when the provider's own mapping is right for the model. */
  reasoning?: SharedLevel;
  /** Provider options to merge over the request's own. */
  providerOptions?: ProviderOptions;
  /** The visible-output ceiling, when thinking is carved out of the request's (Bedrock budget models). */
  maxOutputTokens?: number;
}

/** The highest of `supported` that is not above `level`; the lowest of them when all are. */
function clampLevel(level: ReasoningLevel, supported: readonly ReasoningLevel[]): ReasoningLevel {
  const rank = LADDER.indexOf(level);
  let best: ReasoningLevel | undefined;
  for (const candidate of supported) {
    const candidateRank = LADDER.indexOf(candidate);
    if (candidateRank <= rank && (best === undefined || candidateRank > LADDER.indexOf(best))) best = candidate;
  }
  if (best !== undefined) return best;
  return [...supported].sort((a, b) => LADDER.indexOf(a) - LADDER.indexOf(b))[0] ?? level;
}

/** `level` as the shared option, when it has a shared name. */
function shared(level: ReasoningLevel): SharedLevel | undefined {
  return level === "max" ? undefined : level;
}

// ------------------------------------------------------------------ Claude

/**
 * How a Claude model thinks, mirroring `@ai-sdk/anthropic` `getModelCapabilities`
 * (Bedrock uses the same table): adaptive thinking with an effort that has
 * `xhigh`, the 4.6 models' adaptive thinking without it, or an explicit budget.
 * Every Claude the SDK does not know is adaptive with `xhigh`, as it assumes;
 * an id that is not a Claude at all thinks by budget.
 */
type ClaudeThinking = "adaptive" | "adaptive-without-xhigh" | "budget";

function claudeThinking(modelId: string): ClaudeThinking {
  const id = modelId.toLowerCase();
  if (/claude-(?:opus-5|opus-4-[78]|fable-5|sonnet-5)/.test(id)) return "adaptive";
  if (/claude-(?:opus|sonnet)-4-6/.test(id)) return "adaptive-without-xhigh";
  if (/claude-(?:(?:sonnet|opus|haiku)-4-5|opus-4-1|sonnet-4-|opus-4-|3-|instant|v?2(?:$|[-.:]))/.test(id)) return "budget";
  return id.includes("claude-") ? "adaptive" : "budget";
}

/** Levels a Claude model's effort takes: Anthropic's effort documentation. */
function claudeLevels(thinking: ClaudeThinking): readonly ReasoningLevel[] {
  return thinking === "adaptive-without-xhigh" ? ["low", "medium", "high", "max"] : LADDER;
}

/** Thinking shown as summaries, which the SDK's own adaptive mapping also asks for. */
const ADAPTIVE_THINKING = { type: "adaptive", display: "summarized" } as const;

function anthropicPlan(modelId: string, level: ReasoningLevel): ReasoningPlan {
  const thinking = claudeThinking(modelId);
  // Budget models have no effort to name: `max` is the largest budget the
  // SDK's share table has, the same as `xhigh`.
  if (thinking === "budget") return { reasoning: shared(level) ?? "xhigh" };
  const effort = clampLevel(level, claudeLevels(thinking));
  // `max` has no shared name, and naming the effort in provider options skips
  // the SDK's mapping altogether, so adaptive thinking has to be named with it.
  // `xhigh` on a 4.6 model never reaches the SDK: it would send `max`.
  return effort === "max"
    ? { providerOptions: { anthropic: { effort, thinking: ADAPTIVE_THINKING } } }
    : { reasoning: effort };
}

// ------------------------------------------------------------------ OpenAI

/**
 * Efforts an OpenAI model takes on the Responses API, from OpenAI's model pages:
 * `max` from GPT-5.6 and GPT-6, `xhigh` from GPT-5.2 and the Codex-Max models,
 * `low`–`high` before that and on the o-series, `high` alone on GPT-5 Pro. An id
 * that names no OpenAI reasoning model gets `low`–`high`: the SDK only sends an
 * effort for ids it recognizes (`^o\d`, `^gpt-5`…), and a relay's own names are
 * best not pushed past `high`.
 */
function openaiLevels(modelId: string): readonly ReasoningLevel[] {
  const id = modelId.toLowerCase();
  const gpt = /^gpt-(\d+)(?:\.(\d+))?/.exec(id);
  if (!gpt) return UP_TO_HIGH;
  const major = Number(gpt[1]);
  const minor = Number(gpt[2] ?? 0);
  if (major >= 6) return LADDER;
  if (major < 5) return UP_TO_HIGH;
  if (/^gpt-5-pro/.test(id)) return ["high"];
  if (minor >= 6) return LADDER;
  if (minor >= 2 || id.includes("codex-max")) return UP_TO_XHIGH;
  return UP_TO_HIGH;
}

/** Responses families: `openai-responses`, `openai-codex` (both `openai`) and `azure`. */
function responsesPlan(optionsKey: "openai" | "azure", modelId: string, level: ReasoningLevel): ReasoningPlan {
  const effort = clampLevel(level, openaiLevels(modelId));
  // The Responses provider passes the effort through as written, and its own
  // option takes `max` where the shared one cannot.
  return effort === "max"
    ? { providerOptions: { [optionsKey]: { reasoningEffort: effort } } }
    : { reasoning: effort };
}

/** Chat models from OpenAI that take no `reasoning_effort` at all: any value is a `400`. */
const OPENAI_NON_REASONING_CHAT = /^(?:gpt-[34]|chatgpt-)/;

/**
 * Chat Completions families, which may be any endpoint. OpenAI's own models are
 * held to their ladder, without `max` (documented for Responses only). DeepSeek
 * takes every level and maps it itself. Anything else gets the level asked for:
 * no value is safe everywhere, and `chat-dialect.ts` steps down to a value the
 * endpoint names when it rejects one. The effort rides in `openaiCompatible`
 * options, which pass it through verbatim, `max` included.
 */
function chatPlan(modelId: string, level: ReasoningLevel): ReasoningPlan {
  const id = modelId.toLowerCase().replace(/^openai\//, "");
  if (OPENAI_NON_REASONING_CHAT.test(id)) return {};
  const openai = /^(?:gpt-|o\d)/.test(id);
  const effort = openai ? clampLevel(level, openaiLevels(id).filter((candidate) => candidate !== "max")) : level;
  return { providerOptions: { openaiCompatible: { reasoningEffort: effort } } };
}

// ------------------------------------------------------------------ Others

/**
 * Gemini thinks at `low`, `medium` or `high` (a budget on 2.5, which the SDK
 * sizes from the level); Gemini 3 Pro Preview at `low` or `high` only.
 */
function googlePlan(modelId: string, level: ReasoningLevel): ReasoningPlan {
  const levels = /gemini-3-pro-preview/.test(modelId.toLowerCase()) ? (["low", "high"] as const) : UP_TO_HIGH;
  return { reasoning: shared(clampLevel(level, levels)) };
}

/**
 * Grok from 4.3 takes `low`–`high` and, on the models that have it, `xhigh`;
 * xAI treats `xhigh` as `high` elsewhere rather than refusing it, so it is sent
 * through the provider's own option, which the SDK otherwise narrows to
 * `grok-4.6` alone. Grok 3 Mini takes `low` or `high`. Older Grok models refuse
 * the parameter, and the 4.20 (non-)reasoning pair has none; both get nothing.
 */
function xaiPlan(modelId: string, level: ReasoningLevel): ReasoningPlan {
  const id = modelId.toLowerCase();
  if (/^grok-3-mini/.test(id)) return { reasoning: shared(clampLevel(level, ["low", "high"])) };
  if (/^grok-4\.20(?:-\d{4})?-(?:non-)?reasoning$/.test(id)) return {};
  const grok = /^grok-(\d+)(?:\.(\d+))?/.exec(id);
  const current = grok && (Number(grok[1]) > 4 || (Number(grok[1]) === 4 && Number(grok[2] ?? 0) >= 3));
  if (!current) return {};
  return { providerOptions: { xai: { reasoningEffort: clampLevel(level, UP_TO_XHIGH) } } };
}

/** Each level's share of the output ceiling as a thinking budget, as `@ai-sdk/provider-utils` sizes them. */
const BUDGET_SHARE: Record<ReasoningLevel, number> = { low: 0.1, medium: 0.3, high: 0.6, xhigh: 0.9, max: 0.9 };
/** Anthropic's smallest accepted `budget_tokens`. */
const MIN_THINKING_BUDGET = 1024;

/**
 * Bedrock. Claude takes the same efforts as on Anthropic, named in Bedrock's
 * `reasoningConfig` (the SDK would turn `xhigh` into `max` for every model). A
 * Claude that thinks by budget gets its budget carved out of the request's
 * output ceiling: the SDK adds it on top unclamped, past the model's limit.
 * Nova 2 and gpt-oss take `low`–`high`; Nova 2 refuses `high` while the request
 * sets a token ceiling, which the SDK always does, so it stops at `medium`.
 * Every other model refuses a `reasoningConfig` outright and gets none.
 */
function bedrockPlan(modelId: string, level: ReasoningLevel, maxOutputTokens: number | undefined): ReasoningPlan {
  const id = modelId.toLowerCase();
  // The SDK treats an id as Claude by its `anthropic` vendor prefix, and only
  // then sends Claude's thinking fields rather than a generic `reasoningConfig`.
  if (id.includes("anthropic") && id.includes("claude")) {
    const thinking = claudeThinking(id.slice(id.indexOf("claude")));
    if (thinking !== "budget") {
      return {
        providerOptions: {
          bedrock: {
            reasoningConfig: { ...ADAPTIVE_THINKING, maxReasoningEffort: clampLevel(level, claudeLevels(thinking)) },
          },
        },
      };
    }
    if (maxOutputTokens === undefined) return { reasoning: shared(level) ?? "xhigh" };
    const budgetTokens = Math.max(MIN_THINKING_BUDGET, Math.round(maxOutputTokens * BUDGET_SHARE[level]));
    // The visible part keeps at least one token; a ceiling below the minimum
    // budget grows by what the budget needs, as Anthropic's own rule requires.
    return {
      providerOptions: { bedrock: { reasoningConfig: { type: "enabled", budgetTokens } } },
      maxOutputTokens: Math.max(1, maxOutputTokens - budgetTokens),
    };
  }
  if (id.includes("nova-2")) return { reasoning: shared(clampLevel(level, ["low", "medium"])) };
  if (id.includes("gpt-oss")) return { reasoning: shared(clampLevel(level, UP_TO_HIGH)) };
  return {};
}

// ------------------------------------------------------------------ Entry

/**
 * How a step's `level` reaches `family`'s model `modelId`. `maxOutputTokens` is
 * the visible-output ceiling the step would otherwise send.
 */
export function planReasoning(
  family: StepRequest["family"],
  modelId: string,
  level: ReasoningLevel | undefined,
  maxOutputTokens: number | undefined,
): ReasoningPlan {
  if (level === undefined) return {};
  switch (family) {
    case "anthropic":
      return anthropicPlan(modelId, level);
    case "openai-responses":
    case "openai-codex":
      return responsesPlan("openai", modelId, level);
    case "azure":
      return responsesPlan("azure", modelId, level);
    case "openai-chat":
    case "openai-compatible":
      return chatPlan(modelId, level);
    case "google":
    case "vertex":
      return googlePlan(modelId, level);
    case "xai":
      return xaiPlan(modelId, level);
    case "bedrock":
      return bedrockPlan(modelId, level, maxOutputTokens);
    case "claude-agent":
      // Not an AI SDK call: `claude-agent.ts` passes the level to Claude Code.
      return {};
    default: {
      const exhaustive: never = family;
      throw new Error(`未知的适配家族：${String(exhaustive)}`);
    }
  }
}

/** `base` with `patch` merged in, one level deep per provider key; neither is mutated. */
export function mergeProviderOptions(
  base: Record<string, unknown> | undefined,
  patch: ProviderOptions | undefined,
): Record<string, unknown> | undefined {
  if (!patch) return base;
  const merged: Record<string, unknown> = { ...(base ?? {}) };
  for (const [key, options] of Object.entries(patch)) {
    const existing = merged[key];
    merged[key] = typeof existing === "object" && existing !== null && !Array.isArray(existing)
      ? { ...(existing as Record<string, unknown>), ...options }
      : options;
  }
  return merged;
}
