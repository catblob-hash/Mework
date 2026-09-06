import { describe, expect, it } from "vitest";
// Vite's `?raw` import loads a module as plain text. The send path is checked as
// source rather than executed: `sendPipeline.ts` is imported only by `App.tsx`
// and reaching these gates at runtime would need a full fake host.
import appSource from "../App.tsx?raw";
import sendPipelineSource from "./sendPipeline.ts?raw";

/**
 * The send-path files whose provider readiness gates must go through
 * `hasUsableBaseUrl`, with the number of gates each one owns.
 *
 * Providers whose chat endpoint is derived rather than stored keep
 * `baseUrl: ""` on purpose (`derivesBaseUrl`: vertex, bedrock, openai_codex,
 * claude_agent). A gate written as the raw `!provider.baseUrl.trim()` therefore
 * reads those providers as unconfigured: the composer bounced an OpenAI Codex
 * (OAuth) provider to the settings page and silently dropped the message.
 *
 * `ProviderSettings/ProviderOptionsDrawer.tsx` is deliberately outside this
 * scan. Its `baseUrl.trim()` is protocol-switch normalization — it decides
 * whether a stored endpoint should be rewritten — not a readiness gate.
 */
const SEND_PATH_FILES = [
  // sendComposer, wakeConversation, answer-pending-question, queued promotion.
  { name: "src/lib/sendPipeline.ts", source: sendPipelineSource, gates: 4 },
  // retryActiveModelRun.
  { name: "src/App.tsx", source: appSource, gates: 1 },
] as const;

/**
 * Lines that reach the `baseUrl` field directly. The sanctioned predicate
 * spells it `hasUsableBaseUrl` with a capital `B`, so this case-sensitive scan
 * for the property name never matches the predicate itself. Banning the whole
 * field rather than just `.trim()` also covers the equivalent regressions
 * (`=== ""`, `!provider.baseUrl`, `.baseUrl.length`).
 */
function rawBaseUrlReads(source: string): string[] {
  return source
    .split("\n")
    .map((line, index) => ({ number: index + 1, text: line }))
    .filter((line) => /\bbaseUrl\b/u.test(line.text))
    .map((line) => `${line.number}: ${line.text.trim()}`);
}

describe("send-path provider readiness gates", () => {
  it.each(SEND_PATH_FILES)("$name reads no base URL outside hasUsableBaseUrl", ({ source }) => {
    expect(rawBaseUrlReads(source)).toEqual([]);
  });

  /**
   * Proves the scan's premise. Deleting or renaming the gates would empty the
   * scan above and leave it passing over code that no longer exists, so pin the
   * call sites too: a changed count must be re-justified here.
   */
  it.each(SEND_PATH_FILES)("$name keeps all $gates of its gates", ({ source, gates }) => {
    expect(source.match(/hasUsableBaseUrl\(/gu) ?? []).toHaveLength(gates);
    expect(source).toMatch(/import \{[^}]*\bhasUsableBaseUrl\b[^}]*\} from "[^"]*modelCapabilities"/u);
  });
});
