import type { JSONObject } from "@ai-sdk/provider";
import type { OpenAICompatibleProviderSettings } from "@ai-sdk/openai-compatible";
import { createNullLanguageModelUsage } from "@ai-sdk/provider-utils";

/** Preserve SDK accounting, accepting validated legacy DeepSeek cache evidence. */
export const convertChatUsage: NonNullable<OpenAICompatibleProviderSettings["convertUsage"]> = (usage) => {
  if (usage == null) return createNullLanguageModelUsage();
  const promptTokens = usage.prompt_tokens ?? 0;
  const completionTokens = usage.completion_tokens ?? 0;
  const validCount = (value: unknown): value is number =>
    typeof value === "number" && Number.isFinite(value) && Number.isInteger(value) && value >= 0;
  const hit = usage.prompt_cache_hit_tokens;
  const miss = usage.prompt_cache_miss_tokens;
  const legacyCache = validCount(usage.prompt_tokens) && validCount(hit) && hit <= promptTokens
    && (miss === undefined || (validCount(miss) && hit + miss === promptTokens)) ? hit : 0;
  const cacheRead = usage.prompt_tokens_details?.cached_tokens ?? legacyCache;
  const reasoning = usage.completion_tokens_details?.reasoning_tokens ?? 0;
  return {
    inputTokens: { total: promptTokens, noCache: promptTokens - cacheRead, cacheRead, cacheWrite: undefined },
    outputTokens: { total: completionTokens, text: Math.max(0, completionTokens - reasoning), reasoning },
    raw: usage as JSONObject,
  };
};
