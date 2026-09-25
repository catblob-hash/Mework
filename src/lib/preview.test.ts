import { describe, expect, it, vi } from "vitest";
// Vite's ?raw import loads the authoritative Rust sources as plain text, so this suite compares
// the two sides of the language boundary without compiling anything.
import browserSource from "../../src-tauri/src/browser.rs?raw";
import builtinSchemasSource from "../../src-tauri/src/builtin_schemas.rs?raw";
import previewSource from "../../src-tauri/src/preview.rs?raw";
import previewServersSource from "../../src-tauri/src/preview_servers.rs?raw";
import * as backend from "./backend";
import {
  listPreviewConfigurations,
  listPreviewServers,
  PREVIEW_DEFAULT_LOG_LINES,
  PREVIEW_EMPTY_LOG_REPLIES,
  PREVIEW_MAX_LOG_LINES,
  previewLogLines,
  previewLogSeverity,
  previewServerAddress,
  readPreviewServerLogs,
  startPreviewServer,
  stopPreviewServer
} from "./preview";

const target = { conversationId: "conv-1", workspace: 2 } as const;

function stubBackend<T>(value: T) {
  vi.spyOn(backend, "hasBackendRuntime").mockReturnValue(true);
  return vi.spyOn(backend, "invoke").mockResolvedValue(value as never);
}

describe("preview commands", () => {
  it("sends each command the argument names the host declares", async () => {
    const invoke = stubBackend({});

    await listPreviewConfigurations(target);
    await listPreviewServers(target);
    await startPreviewServer(target, "web");
    await startPreviewServer(target);
    await stopPreviewServer("srv-1");
    await readPreviewServerLogs("srv-1", { errorsOnly: true, search: "vite", lines: 200 });
    await readPreviewServerLogs("srv-1");

    expect(invoke.mock.calls).toEqual([
      ["preview_list_configurations", { target }],
      ["preview_list_servers", { target }],
      ["preview_start_server", { target, name: "web" }],
      // An unnamed start is the host's "whichever one this project configures", not a server
      // literally named "": the null has to survive the wire.
      ["preview_start_server", { target, name: null }],
      ["preview_stop_server", { handle: "srv-1" }],
      ["preview_server_logs", { handle: "srv-1", errorsOnly: true, search: "vite", lines: 200 }],
      ["preview_server_logs", { handle: "srv-1", errorsOnly: null, search: null, lines: null }]
    ]);
  });

  it("refuses every preview command outside the desktop application", async () => {
    vi.spyOn(backend, "hasBackendRuntime").mockReturnValue(false);

    await expect(listPreviewConfigurations(target)).rejects.toThrow(/桌面应用/);
    await expect(stopPreviewServer("srv-1")).rejects.toThrow(/桌面应用/);
  });

  it("prefers a configured url over the port it was started on", () => {
    expect(previewServerAddress({ port: 5173 })).toBe("http://localhost:5173");
    expect(previewServerAddress({ port: 5173, url: "  " })).toBe("http://localhost:5173");
    expect(previewServerAddress({ port: 8443, url: "https://localhost:8443" }))
      .toBe("https://localhost:8443");
  });

  it("turns the host's whole-buffer replies into drawer lines", () => {
    expect(previewLogLines("")).toEqual([]);
    expect(previewLogLines('No logs matching "vite".')).toEqual([]);
    // The term is quoted verbatim, quotes and all, so the match cannot stop at the first one.
    expect(previewLogLines('No logs matching "a "quoted" term".')).toEqual([]);
    // A real line that merely starts the same way is still a line.
    expect(previewLogLines("No logs matching in build output.")).toEqual([
      "No logs matching in build output."
    ]);
    // Entries are raw pipe chunks carrying their own newlines, so the trailing one is not a line.
    expect(previewLogLines("ready\r\nlistening on 5173\n")).toEqual(["ready", "listening on 5173"]);
  });

  it("classifies a log line from its first 200 characters, uppercased", () => {
    expect(previewLogSeverity("vite ready in 300 ms")).toBeNull();
    expect(previewLogSeverity("npm ERR! missing script")).toBe("error");
    expect(previewLogSeverity("Build failed")).toBe("error");
    expect(previewLogSeverity("FATAL: port in use")).toBe("error");
    expect(previewLogSeverity("warn: slow build")).toBe("warn");
    // Past the 200-character window the classifier stops looking, exactly as the source does.
    expect(previewLogSeverity(`${"x".repeat(200)} ERROR`)).toBeNull();
  });
});

/** Slices the `{ … }` block starting at `start`, skipping over Rust string literals. */
function balancedBlock(source: string, start: number): string {
  let depth = 0;
  let index = start;
  while (index < source.length) {
    const char = source[index];
    if (char === '"') {
      index += 1;
      while (index < source.length && source[index] !== '"') {
        index += source[index] === "\\" ? 2 : 1;
      }
    } else if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(start, index + 1);
    }
    index += 1;
  }
  throw new Error(`unbalanced braces from offset ${start}`);
}

function unescapeRustLiteral(literal: string): string {
  return literal.replace(/\\(u\{([0-9a-fA-F]+)\}|.)/gu, (_match, sequence: string, code?: string) => {
    if (code) return String.fromCodePoint(Number.parseInt(code, 16));
    if (sequence === "n") return "\n";
    if (sequence === "t") return "\t";
    if (sequence === "0") return "\0";
    return sequence;
  });
}

function rustStringLiterals(block: string): string[] {
  return [...block.matchAll(/"((?:[^"\\]|\\.)*)"/gu)].map(([, body]) => unescapeRustLiteral(body));
}

function rustU32(source: string, file: string, name: string): number {
  const match = new RegExp(
    `(?<![A-Za-z0-9_])const ${name}:\\s*u32\\s*=\\s*([0-9_]+)\\s*;`,
    "u"
  ).exec(source);
  if (!match) throw new Error(`${file}: could not read \`const ${name}: u32\``);
  return Number(match[1].replaceAll("_", ""));
}

/**
 * Every whole-buffer reply the host can answer `preview_server_logs` with, read out of the Rust
 * that produces them. `{search}` is left as the format placeholder it is.
 */
function hostEmptyLogReplies(): string[] {
  const render = previewServersSource.indexOf("pub fn render_preview_logs(");
  if (render < 0) throw new Error("preview_servers.rs: `render_preview_logs` is gone");
  const branch = previewServersSource.indexOf("if tail.is_empty() {", render);
  if (branch < 0) {
    throw new Error("preview_servers.rs: `render_preview_logs` no longer answers an empty tail");
  }
  const constant = /pub const NO_SERVER_FOR_LOGS:\s*&str\s*=\s*"((?:[^"\\]|\\.)*)"\s*;/u
    .exec(previewSource);
  if (!constant) throw new Error("preview.rs: could not read `NO_SERVER_FOR_LOGS`");
  return [
    ...rustStringLiterals(balancedBlock(previewServersSource, previewServersSource.indexOf("{", branch))),
    unescapeRustLiteral(constant[1])
  ];
}

// Nothing else reconciles these two sides. The drawer decides it has no lines to show by
// comparing the host's reply against text copied out of Rust by hand, and the limits below are
// spelled out once per language — so rewording or renumbering one side alone compiles, passes
// every other test, and only shows up as a host sentence printed as though a dev server had
// emitted it, or a request the host silently clamps.
describe("preview constants pinned to the Rust that produces them", () => {
  it("recognises every empty reply the host can send as an empty reply", () => {
    const replies = hostEmptyLogReplies();

    // Three from `render_preview_logs`, one from `NO_SERVER_FOR_LOGS`. Without this a parser
    // that matched nothing would make the loop below vacuous.
    expect(replies).toHaveLength(4);
    for (const reply of replies) {
      // The one templated reply quotes the search term back; any term stands in for it here.
      const rendered = reply.replace("{search}", "vite");
      expect(previewLogLines(rendered), rendered).toEqual([]);
    }
  });

  it("keeps no reply the host has stopped sending", () => {
    const replies = hostEmptyLogReplies();

    for (const reply of PREVIEW_EMPTY_LOG_REPLIES) {
      expect(replies, reply).toContain(reply);
    }
    // The fourth is the templated one, which the regex covers rather than this list.
    expect(PREVIEW_EMPTY_LOG_REPLIES).toHaveLength(replies.length - 1);
  });

  it("keeps one log-line ceiling and one default across all three spellings", () => {
    const ceiling = rustU32(previewServersSource, "preview_servers.rs", "MAX_LOG_LINES");

    expect(ceiling).toBe(rustU32(builtinSchemasSource, "builtin_schemas.rs", "PREVIEW_MAX_LOG_LINES"));
    expect(PREVIEW_MAX_LOG_LINES).toBe(ceiling);
    expect(PREVIEW_DEFAULT_LOG_LINES).toBe(
      rustU32(previewServersSource, "preview_servers.rs", "DEFAULT_LOG_LINES")
    );
  });

  it("keeps one viewport ceiling across both spellings", () => {
    expect(rustU32(builtinSchemasSource, "builtin_schemas.rs", "PREVIEW_MAX_VIEWPORT")).toBe(
      rustU32(browserSource, "browser.rs", "PREVIEW_VIEWPORT_MAX")
    );
  });
});
