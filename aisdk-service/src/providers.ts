//! Provider-family to AI SDK-provider mapping.
//!
//! This is the only provider-family dispatch point and is a total mapping from Rust's
//! `ProviderFamily`.
//!
//! - `baseURL` has already passed host URL validation. The sidecar must not duplicate
//!   that authority or create drifting validation rules.
//! - API keys come only from stdin. Providers may otherwise read environment variables
//!   such as `OPENAI_API_KEY`, which could silently send an unrelated development key
//!   to a user-configured relay. An explicit empty string lets the upstream return 401.
//! - Responses-compatible endpoints use `createOpenAI`, which preserves access to
//!   `openai.tools.webSearch()`; the generic OpenAI-compatible provider discards
//!   provider-defined tools.

import { createAmazonBedrock } from "@ai-sdk/amazon-bedrock";
import { createAnthropic } from "@ai-sdk/anthropic";
import { createAzure } from "@ai-sdk/azure";
import { createGoogle } from "@ai-sdk/google";
import { createGoogleVertex } from "@ai-sdk/google-vertex";
import { createOpenAI } from "@ai-sdk/openai";
import { createOpenAICompatible } from "@ai-sdk/openai-compatible";
import { createXai } from "@ai-sdk/xai";
import type { LanguageModel } from "ai";

import { anthropicDialectFetch } from "./anthropic-dialect.js";
import { chatDialectFetch } from "./chat-dialect.js";
import { convertChatUsage } from "./chat-usage.js";
import { codexDialectFetch } from "./codex-dialect.js";
import type { ProviderFamily } from "./protocol.js";
import { plaintextReasoningFetch } from "./responses-dialect.js";

export interface ProviderTarget {
  family: ProviderFamily;
  /** Host-computed base URL. Omission uses the provider default; Vertex and Bedrock
   * derive endpoints from `settings` project, location, or region values. */
  baseURL?: string;
  apiKey?: string;
  headers?: Record<string, string>;
  /** Family-specific identity fields: `region`, `project`, `location`, or `apiVersion`. */
  settings?: Record<string, string>;
  modelId: string;
  /** Model reasoning representation. Omission means no consumer.
   * Only `plaintext` changes behavior by translating Responses reasoning SSE. */
  reasoningContent?: "plaintext" | "encrypted";
  /** The model's prompt-cache attribute; `false` turns Anthropic breakpoints off. */
  promptCache?: boolean;
  /** The per-step system-prompt tail, the Anthropic dialect's cache boundary. */
  systemDynamic?: string;
}

/** Provider instance and language model used by this request. `provider` supplies
 * native tool factories to `search.ts`. */
export interface ResolvedModel {
  model: LanguageModel;
  /** Host object for native tool factories; `undefined` means no provider-defined tools. */
  provider: unknown;
}

export function resolveModel(target: ProviderTarget): ResolvedModel {
  const { baseURL, modelId, headers } = target;
  // Use an explicit empty string rather than `undefined`, which lets providers read
  // environment variables.
  const apiKey = target.apiKey ?? "";
  const settings = target.settings ?? {};
  // `baseURL: undefined` selects the provider default, while `baseURL: ""` produces
  // an invalid relative URL. The host rejects empty strings; this is a second guard.
  const base = baseURL && baseURL.length > 0 ? baseURL : undefined;
  // Plaintext models may use DeepSeek's reasoning_text response dialect.
  // Encrypted replay includes are retained independently of presentation mode.
  const responsesFetch = target.reasoningContent === "plaintext"
    ? plaintextReasoningFetch()
    : undefined;

  switch (target.family) {
    case "openai-responses": {
      const provider = createOpenAI({ baseURL: base, apiKey, headers, fetch: responsesFetch });
      return { model: provider.responses(modelId), provider };
    }
    case "openai-codex": {
      // ChatGPT subscription Codex requires streamed Responses requests; its
      // dialect wrapper also turns completed SSE back into the JSON response a
      // non-streaming AI SDK call expects.
      const provider = createOpenAI({
        baseURL: base ?? "https://chatgpt.com/backend-api/codex",
        apiKey,
        headers,
        fetch: codexDialectFetch(),
      });
      return { model: provider.responses(modelId), provider };
    }
    case "openai-chat": {
      // Use OpenAI-compatible rather than `createOpenAI().chat()`: the latter carries
      // OpenAI-specific capability tables and parameter choices, while this branch
      // serves every Chat Completions-compatible upstream.
      //
      // `includeUsage` requests the final usage block required by strict Chat
      // Completions endpoints. The fetch wrapper normalizes Chat dialect differences.
      const provider = createOpenAICompatible({
        name: "openai-chat",
        baseURL: base ?? "",
        apiKey,
        headers,
        includeUsage: true,
        convertUsage: convertChatUsage,
        fetch: chatDialectFetch(),
      });
      return { model: provider.chatModel(modelId), provider: undefined };
    }
    case "anthropic": {
      // Normalize compatible endpoints that wrap server-search error objects in an
      // array, and rewrite adaptive thinking into an explicit budget for endpoints
      // that silently drop it. Both are no-ops for official Anthropic. The cache
      // directives are per request, which is why the wrapper is built here rather
      // than shared across requests.
      const provider = createAnthropic({
        baseURL: base,
        apiKey,
        headers,
        fetch: anthropicDialectFetch(base, globalThis.fetch, {
          enabled: target.promptCache,
          systemDynamic: target.systemDynamic,
        }),
      });
      return { model: provider(modelId), provider };
    }
    case "claude-agent": {
      // Not an HTTP dialect: `claude-agent.ts` drives the Claude Code executable
      // through the Agent SDK and `main.ts` dispatches to it before reaching here.
      throw new Error("claude-agent 家族不经 AI SDK 模型解析");
    }
    case "google": {
      const provider = createGoogle({ baseURL: base, apiKey, headers });
      return { model: provider(modelId), provider };
    }
    case "xai": {
      const provider = createXai({ baseURL: base, apiKey, headers });
      return { model: provider(modelId), provider };
    }
    case "azure": {
      // Azure identity is resource plus deployment. Users supply the resource-specific
      // `baseURL`; absent `apiVersion` uses the AI SDK default.
      const provider = createAzure({
        baseURL: base,
        apiKey,
        headers,
        fetch: responsesFetch,
        ...(settings.apiVersion ? { apiVersion: settings.apiVersion } : {}),
      });
      return { model: provider(modelId), provider };
    }
    case "bedrock": {
      // `region` is required and derives the endpoint host, so `baseURL` is normally
      // absent.
      const provider = createAmazonBedrock({
        baseURL: base,
        apiKey,
        headers,
        ...(settings.region ? { region: settings.region } : {}),
      });
      return { model: provider(modelId), provider };
    }
    case "vertex": {
      // `project` and `location` are required; Vertex derives its endpoint and model
      // path from them.
      const provider = createGoogleVertex({
        baseURL: base,
        apiKey,
        headers,
        ...(settings.project ? { project: settings.project } : {}),
        ...(settings.location ? { location: settings.location } : {}),
      });
      return { model: provider(modelId), provider };
    }
    case "openai-compatible": {
      // `includeUsage` and `fetch` follow the same requirements as `openai-chat`.
      const provider = createOpenAICompatible({
        name: "openai-compatible",
        baseURL: base ?? "",
        apiKey,
        headers,
        includeUsage: true,
        convertUsage: convertChatUsage,
        fetch: chatDialectFetch(),
      });
      return { model: provider.chatModel(modelId), provider: undefined };
    }
    default: {
      // Exhaustive dispatch: adding a family without handling it here is a TypeScript
      // `never` error.
      const exhaustive: never = target.family;
      throw new Error(`未知的适配家族：${String(exhaustive)}`);
    }
  }
}
