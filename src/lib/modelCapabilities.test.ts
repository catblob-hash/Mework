import { describe, expect, it } from "vitest";
import modelSource from "../../src-tauri/src/model.rs?raw";
import type { ProviderFamily, ModelProfile } from "../types";
import {
  CHAT_ENDPOINT_TYPES,
  chatEndpointOf,
  derivesBaseUrl,
  ENDPOINT_TYPES,
  hasUsableBaseUrl,
  isEncryptedReasoning,
  MODEL_CAPABILITIES,
  modelGroup,
  normalizeCapabilities,
  normalizeEndpointTypes,
  normalizeReasoningContent,
  reasoningContentTakesEffect,
  REASONING_CONTENTS,
  repairActiveModelId,
} from "./modelCapabilities";

function model(id: string, group = ""): ModelProfile {
  return { id, name: "", group, capabilities: [], reasoningContent: "plaintext" };
}

describe("modelGroup", () => {
  it("prefers an explicit group over anything inferred", () => {
    expect(modelGroup(model("Qwen/Qwen3-8B", "  我的分组  "))).toBe("我的分组");
  });

  // This table mirrors `model_registry.rs::derive_model_group_name` exactly.
  // Discovery writes `group`; this fallback must use the same rule so models do
  // not move between collapsed groups before and after discovery.
  it.each([
    ["openai/gpt-4o", "openai"],
    ["Qwen/Qwen3-8B", "Qwen"],
    ["deepseek-ai/DeepSeek-V3", "deepseek-ai"],
    ["deepseek-v4-pro", "deepseek"],
    ["gpt-4o-mini", "gpt"],
    ["claude-sonnet-4-5", "claude"],
    // An id whose first segment is the whole id has no group; the renderer puts it in Other.
    ["grok", ""],
    ["o3", ""],
    ["", ""]
  ])("derives %s → %s", (id, expected) => {
    expect(modelGroup(model(id))).toBe(expected);
  });
});

/** Extracts the body of an `impl <Name> {` block. Brace counting skips quoted
 * strings because comments may contain braces. */
function implBlock(source: string, name: string): string {
  const header = `impl ${name} {`;
  const start = source.indexOf(header);
  expect(start, `model.rs 里找不到 impl ${name}`).toBeGreaterThan(-1);
  let depth = 0;
  let inString = false;
  for (let index = start + header.length - 1; index < source.length; index += 1) {
    const char = source[index];
    if (inString) {
      if (char === "\\") index += 1;
      else if (char === '"') inString = false;
      continue;
    }
    if (char === '"') inString = true;
    else if (char === "{") depth += 1;
    else if (char === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(start, index);
    }
  }
  throw new Error(`impl ${name} 没有闭合`);
}

/** Mapping of `Self::Variant => "slug",`. */
function slugMap(block: string): Map<string, string> {
  return new Map(
    Array.from(block.matchAll(/Self::(\w+)\s*=>\s*"([^"]+)"/gu), (match) => [match[1], match[2]] as const)
  );
}

/** Variant order in a `pub const NAME: &'static [Self] = &[ Self::A, Self::B ];`. */
function constVariants(block: string, name: string): string[] {
  const pattern = new RegExp(`const ${name}:[^=]+=\\s*&\\[([^\\]]*)\\]`, "u");
  const match = block.match(pattern);
  expect(match, `找不到 ${name}`).toBeTruthy();
  return Array.from((match as RegExpMatchArray)[1].matchAll(/Self::(\w+)/gu), (entry) => entry[1]);
}

function rustCatalog(name: string, constName: string): string[] {
  const block = implBlock(modelSource, name);
  const slugs = slugMap(block);
  return constVariants(block, constName).map((variant) => {
    const slug = slugs.get(variant);
    expect(slug, `${name}::${variant} 没有 slug`).toBeTruthy();
    return slug as string;
  });
}

/** Variant names declared by `pub enum <Name> { … }`, skipping docs and attributes. */
function enumVariants(source: string, name: string): string[] {
  const header = `pub enum ${name} {`;
  const start = source.indexOf(header);
  expect(start, `model.rs 里找不到 pub enum ${name}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}", start);
  expect(end, `pub enum ${name} 没有闭合`).toBeGreaterThan(start);
  return source
    .slice(start + header.length, end)
    .split("\n")
    .map((line) => line.trim().replace(/,$/u, ""))
    .filter((line) => /^[A-Z]\w*$/u.test(line));
}

describe("endpoint and capability vocabularies mirror Rust", () => {
  it("mirrors EndpointType::CATALOG exactly, including order", () => {
    expect(ENDPOINT_TYPES).toEqual(rustCatalog("EndpointType", "CATALOG"));
  });

  it("mirrors EndpointType::CHAT exactly, including order", () => {
    expect(CHAT_ENDPOINT_TYPES).toEqual(rustCatalog("EndpointType", "CHAT"));
  });

  it("mirrors ModelCapability::CATALOG exactly, including order", () => {
    expect(MODEL_CAPABILITIES).toEqual(rustCatalog("ModelCapability", "CATALOG"));
  });

  it("mirrors ReasoningContent::CATALOG exactly, including order", () => {
    expect(REASONING_CONTENTS).toEqual(rustCatalog("ReasoningContent", "CATALOG"));
  });

  /**
   * The mirrors above read `CATALOG`, so a Rust variant declared outside its
   * catalog would pass them while still reaching the wire through serde. The
   * capability and reasoning vocabularies were deliberately shrunk, so pin the
   * enum declarations themselves rather than only their catalogs.
   */
  it("leaves no Rust variant outside its catalog", () => {
    for (const name of ["EndpointType", "ModelCapability", "ReasoningContent"]) {
      expect(enumVariants(modelSource, name), name)
        .toEqual(constVariants(implBlock(modelSource, name), "CATALOG"));
    }
  });

  it("mirrors ProviderFamily::chat_endpoint for every format", () => {
    // `Self::OpenaiChat | Self::Xai => EndpointType::OpenaiChatCompletions,`
    // and block-form branches such as `Self::A | Self::B => { EndpointType::C }`.
    //
    // A left-hand side can combine variants with `|`; parse every variant and
    // accept an optional right-hand brace so no branch silently escapes the mirror assertion.
    const block = implBlock(modelSource, "ProviderFamily");
    const rust = new Map<string, string>();
    for (const match of block.matchAll(
      /((?:\s*\|?\s*Self::\w+)+)\s*=>\s*\{?\s*EndpointType::(\w+)/gu
    )) {
      for (const variant of match[1].matchAll(/Self::(\w+)/gu)) {
        rust.set(variant[1], match[2]);
      }
    }
    const endpointSlugs = slugMap(implBlock(modelSource, "EndpointType"));
    const formatSlugs = new Map<string, ProviderFamily>([
      ["OpenaiResponses", "openai_responses"],
      ["OpenaiCodex", "openai_codex"],
      ["OpenaiChat", "openai_chat"],
      ["Anthropic", "anthropic"],
      ["ClaudeAgent", "claude_agent"],
      ["Google", "google"],
      ["Xai", "xai"],
      ["Azure", "azure"],
      ["Bedrock", "bedrock"],
      ["Vertex", "vertex"],
      ["OpenaiCompatible", "openai_compatible"],
    ]);
    expect(rust.size).toBe(formatSlugs.size);
    for (const [variant, endpointVariant] of rust) {
      const format = formatSlugs.get(variant);
      expect(format, `ProviderFamily::${variant} 不在 TS 的格式表里`).toBeTruthy();
      expect(chatEndpointOf(format as ProviderFamily)).toBe(endpointSlugs.get(endpointVariant));
    }
  });

  it("normalizes to catalog order and drops unknown entries", () => {
    // `rerank`, `function_call` and `reasoning` are retired slugs an archived
    // document still carries; they must normalize away rather than survive.
    expect(normalizeCapabilities(["rerank", "function_call", "reasoning", "nope", 7])).toEqual([]);
    expect(normalizeCapabilities(["image_recognition", "image_recognition"]))
      .toEqual(["image_recognition"]);
    expect(normalizeEndpointTypes(["openai_text_to_speech", "openai_responses", "nope"])).toEqual([
      "openai_responses",
      "openai_text_to_speech",
    ]);
  });
});

/** ProviderFamily variant names mapped to persisted slugs, matching Rust `rename_all = "snake_case"`. */
const FAMILY_SLUGS = new Map<string, ProviderFamily>([
  ["OpenaiResponses", "openai_responses"],
  ["OpenaiCodex", "openai_codex"],
  ["OpenaiChat", "openai_chat"],
  ["Anthropic", "anthropic"],
  ["ClaudeAgent", "claude_agent"],
  ["Google", "google"],
  ["Xai", "xai"],
  ["Azure", "azure"],
  ["Bedrock", "bedrock"],
  ["Vertex", "vertex"],
  ["OpenaiCompatible", "openai_compatible"],
]);

describe("reasoning form", () => {
  /**
   * Rust and TypeScript each define which provider families consume reasoning
   * forms. Compare every family against `model.rs` to keep those tables aligned.
   */
  it("mirrors ProviderFamily::reasoning_content_takes_effect for every family", () => {
    const body = /fn reasoning_content_takes_effect\(self\) -> bool \{([\s\S]*?)\n    \}/u
      .exec(modelSource);
    expect(body, "model.rs 里找不到 reasoning_content_takes_effect").toBeTruthy();
    const rustTrue = new Set(
      [...(body as RegExpExecArray)[1].matchAll(/Self::(\w+)/gu)].map((match) => match[1])
    );
    expect(rustTrue.size).toBeGreaterThan(0);
    for (const [variant, family] of FAMILY_SLUGS) {
      expect(reasoningContentTakesEffect(family), `ProviderFamily::${variant}`)
        .toBe(rustTrue.has(variant));
    }
  });

  /**
   * `reasoningContent` is concrete on every model now, so the normalizer — not a
   * later resolver — is where an absent, retired or unrecognized value acquires a
   * form. `"auto"` was the retired third variant and still sits in archives.
   */
  it("resolves an absent, retired or unrecognized value by family", () => {
    for (const family of FAMILY_SLUGS.values()) {
      const expected = reasoningContentTakesEffect(family) ? "encrypted" : "plaintext";
      expect(normalizeReasoningContent(undefined, family), `${family} 缺失`).toBe(expected);
      expect(normalizeReasoningContent("auto", family), `${family} auto`).toBe(expected);
      expect(normalizeReasoningContent("nonsense", family), `${family} 未知`).toBe(expected);
      expect(normalizeReasoningContent("plaintext", family), `${family} plaintext`)
        .toBe("plaintext");
      expect(normalizeReasoningContent("encrypted", family), `${family} encrypted`)
        .toBe("encrypted");
    }
    // The two families below sit on opposite sides of the table, so the loop
    // above cannot pass with a constant fallback.
    expect(normalizeReasoningContent(undefined, "openai_responses")).toBe("encrypted");
    expect(normalizeReasoningContent(undefined, "anthropic")).toBe("plaintext");
  });

  /**
   * A card's `form` is authoritative; only an absent form falls back to content.
   * An empty plaintext card remains editable, unlike an encrypted card.
   */
  it("reads the card's own form first and only then falls back to emptiness", () => {
    expect(isEncryptedReasoning({ form: "encrypted", content: "有摘要" })).toBe(true);
    expect(isEncryptedReasoning({ form: "plaintext", content: "" })).toBe(false);
    expect(isEncryptedReasoning({ form: "plaintext" })).toBe(false);
    // Cards written before `form` existed fall back to the legacy heuristic.
    expect(isEncryptedReasoning({ content: "" })).toBe(true);
    expect(isEncryptedReasoning({})).toBe(true);
    expect(isEncryptedReasoning({ content: "想法" })).toBe(false);
  });
});

describe("chat base URL readiness", () => {
  /**
   * A provider whose chat endpoint is derived keeps `baseUrl: ""` on purpose, so
   * both sides must agree on exactly which families those are. Drift here makes
   * the host and the renderer disagree about whether a provider can send.
   */
  it("mirrors ProviderFamily::derives_base_url for every family", () => {
    const body = /fn derives_base_url\(self\) -> bool \{([\s\S]*?)\n    \}/u.exec(modelSource);
    expect(body, "model.rs 里找不到 derives_base_url").toBeTruthy();
    const rustTrue = new Set(
      [...(body as RegExpExecArray)[1].matchAll(/Self::(\w+)/gu)].map((match) => match[1])
    );
    expect(rustTrue.size).toBeGreaterThan(0);
    for (const [variant, family] of FAMILY_SLUGS) {
      expect(derivesBaseUrl(family), `ProviderFamily::${variant}`).toBe(rustTrue.has(variant));
    }
  });

  /**
   * The deriving families accept an empty Base URL; a key-based family has
   * nowhere to derive an endpoint from, so for it an empty value is genuinely
   * unusable. Whitespace counts as empty on both branches.
   */
  it("accepts an empty base URL only from the deriving families", () => {
    for (const family of ["vertex", "bedrock", "openai_codex", "claude_agent"] as const) {
      expect(hasUsableBaseUrl({ family, baseUrl: "" }), family).toBe(true);
      expect(hasUsableBaseUrl({ family, baseUrl: "   " }), family).toBe(true);
      // An override still points the derived family at a loopback test double.
      expect(hasUsableBaseUrl({ family, baseUrl: "http://127.0.0.1:1420" }), family).toBe(true);
    }
    for (const family of ["openai_chat", "openai_responses", "anthropic"] as const) {
      expect(hasUsableBaseUrl({ family, baseUrl: "" }), family).toBe(false);
      expect(hasUsableBaseUrl({ family, baseUrl: "   " }), family).toBe(false);
      expect(hasUsableBaseUrl({ family, baseUrl: "https://api.openai.com/v1" }), family).toBe(true);
    }
  });
});

describe("active model repair", () => {
  it("keeps the active model untouched while it is still in the list", () => {
    expect(repairActiveModelId({
      models: [model("a"), model("b")],
      activeModelId: "b"
    })).toBe("b");
  });

  it("falls back to the first model once the active one is uninstalled", () => {
    // Presence in the provider's list is the whole of usability now, so the only
    // way to dangle `activeModelId` is to remove the model it names. Two
    // survivors defeat an implementation that reaches for the last entry.
    expect(repairActiveModelId({
      models: [model("first"), model("second")],
      activeModelId: "gone"
    })).toBe("first");
  });

  it("returns null only when the provider has no models left", () => {
    expect(repairActiveModelId({ models: [], activeModelId: "gone" })).toBeNull();
    expect(repairActiveModelId({ models: [], activeModelId: null })).toBeNull();
  });
});
