import { hasBackendRuntime, invoke } from "./backend";
import type { GitTarget } from "./git";

/** Mirror of Rust `preview_servers::PreviewServerStatus`. */
export type PreviewServerStatus = "starting" | "running" | "stopped" | "failed";

/** Mirror of Rust `preview::PreviewConfiguredServer` — one usable `.mework/launch.json` entry. */
export interface PreviewConfiguredServer {
  name: string;
  command: string | null;
  args: string[];
  cwd: string;
  /**
   * Still the parser's `u32`. A port read out of a command line is not range-checked, so a value
   * above 65535 reaches the renderer intact and `preview_start_server` is the one that refuses it.
   */
  port: number;
  autoPort?: boolean | null;
  url?: string | null;
}

/** Mirror of Rust `preview::PreviewMalformedEntry`. */
export interface PreviewMalformedEntry {
  name: string;
  reason: string;
}

/** Mirror of Rust `preview::PreviewConfigurationList`. */
export interface PreviewConfigurationList {
  launchJsonPath: string;
  servers: PreviewConfiguredServer[];
  malformed: PreviewMalformedEntry[];
  autoVerify: boolean;
  /** The full explanation of an unusable file, absent when the file is usable. */
  problem?: string | null;
  problemReason?: string | null;
}

/** Mirror of Rust `preview_servers::PreviewServerSnapshot`. */
export interface PreviewServerSnapshot {
  serverId: string;
  name: string;
  port: number;
  status: PreviewServerStatus;
  startedAt: string;
  /** The worktree the server is registered under, not the process's own directory. */
  cwd: string;
  sessionId?: string | null;
}

/** Mirror of Rust `preview::PreviewAttachment` — a `url` entry with no command to run. */
export interface PreviewAttachment {
  /** Not a process id: it names the preview page, so `preview_stop` and `preview_logs` refuse it. */
  serverId: string;
  name: string;
  /** `0` whenever the entry states no port, which a non-localhost url never does. */
  port: number;
  url: string;
}

/**
 * Mirror of Rust `preview::PreviewStartOutcome`.
 *
 * Untagged, like the host's own enum: an attach started no process, so it has no snapshot to
 * report and is told apart by the member that is only there for it.
 */
export type PreviewStartOutcome =
  | { server: PreviewServerSnapshot; reused: boolean }
  | { attached: PreviewAttachment };

/** Whether a start attached to somebody else's server instead of running one. */
export function isPreviewAttachment(
  outcome: PreviewStartOutcome
): outcome is { attached: PreviewAttachment } {
  return "attached" in outcome;
}

/** Mirror of Rust `preview_servers::PreviewLogQuery`. */
export interface PreviewLogQuery {
  errorsOnly?: boolean;
  search?: string;
  /**
   * Clamped host-side to 1..={@link PREVIEW_MAX_LOG_LINES}; omitted means
   * {@link PREVIEW_DEFAULT_LOG_LINES}.
   */
  lines?: number;
}

/**
 * The host's own line ceiling (`preview_servers::MAX_LOG_LINES`), so it is also the largest
 * request that changes anything. `preview.test.ts` reads the Rust sources to prove this number
 * still matches both places that spell it out there.
 */
export const PREVIEW_MAX_LOG_LINES = 200;

/** What the host uses when `lines` is omitted (`preview_servers::DEFAULT_LOG_LINES`). */
export const PREVIEW_DEFAULT_LOG_LINES = 50;

function requireDesktopRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("开发服务器仅可在桌面应用中使用");
}

/** Everything `.mework/launch.json` says, including what is wrong with it. Never throws host-side. */
export async function listPreviewConfigurations(
  target: GitTarget
): Promise<PreviewConfigurationList> {
  requireDesktopRuntime();
  return invoke<PreviewConfigurationList>("preview_list_configurations", { target });
}

/** The dev servers running for this workspace, whichever conversation started them. */
export async function listPreviewServers(target: GitTarget): Promise<PreviewServerSnapshot[]> {
  requireDesktopRuntime();
  return invoke<PreviewServerSnapshot[]>("preview_list_servers", { target });
}

/**
 * Starts the configured server `name` addresses, or hands back the one already answering it.
 *
 * Blocks until the host has a running process or has given up, so a failure arrives as a rejected
 * promise carrying the spawn diagnosis — which is the text the failure card shows.
 */
export async function startPreviewServer(
  target: GitTarget,
  name?: string
): Promise<PreviewStartOutcome> {
  requireDesktopRuntime();
  return invoke<PreviewStartOutcome>("preview_start_server", { target, name: name ?? null });
}

/** Stops one dev server and forgets it, buffered output included. False means it was already gone. */
export async function stopPreviewServer(serverId: string): Promise<boolean> {
  requireDesktopRuntime();
  return invoke<boolean>("preview_stop_server", { serverId });
}

/** One dev server's buffered output, filtered the way the `preview_logs` tool filters it. */
export async function readPreviewServerLogs(
  serverId: string,
  query: PreviewLogQuery = {}
): Promise<string> {
  requireDesktopRuntime();
  return invoke<string>("preview_server_logs", {
    serverId,
    errorsOnly: query.errorsOnly ?? null,
    search: query.search ?? null,
    lines: query.lines ?? null
  });
}

/**
 * Records whether this project wants its previews verified automatically.
 *
 * False for every reason the write did not happen — a project without a launch.json has nowhere
 * to keep the preference — so callers re-read the configuration rather than trusting the request.
 */
export async function setPreviewAutoVerify(
  target: GitTarget,
  enabled: boolean
): Promise<boolean> {
  requireDesktopRuntime();
  return invoke<boolean>("preview_set_auto_verify", { target, enabled });
}

/** Where a configured or running server answers. */
export function previewServerAddress(server: {
  port: number;
  url?: string | null;
}): string {
  const explicit = server.url?.trim();
  if (explicit) return explicit;
  return `http://localhost:${server.port}`;
}

/**
 * Whether a page at `url` is being served by a server answering at `address`.
 *
 * Origin equality, not string equality: the page navigates within the server —
 * a route, a query, a trailing slash — and every one of those is still that
 * server's page. `about:blank` is nobody's, and a URL neither side can parse
 * matches nothing rather than matching everything.
 */
export function previewUrlIsServedAt(url: string | null | undefined, address: string): boolean {
  const page = url?.trim();
  if (!page || page === "about:blank") return false;
  try {
    return new URL(page).origin === new URL(address).origin;
  } catch {
    return false;
  }
}

/**
 * The whole-buffer replies the host sends instead of output. They are sentences, not log lines, so
 * the drawer shows its own empty state rather than printing them as if a server had said them.
 *
 * Each one is a Rust literal — three from `preview_servers::render_preview_logs`, the last from
 * `preview::NO_SERVER_FOR_LOGS`. `preview.test.ts` reads both sources and fails when the wording
 * on either side moves, because a reply this list no longer recognises reaches the drawer as a
 * line and gets printed as though a dev server had emitted it.
 */
export const PREVIEW_EMPTY_LOG_REPLIES = [
  "No logs yet.",
  "No server errors found.",
  "No dev server is running. preview_logs takes a process serverId from preview_list."
];

/**
 * The fourth reply, which quotes the search term back, so no set of exact strings can hold it.
 * Nothing reaches the drawer with it today — the drawer has no search box — and the host answers
 * with it the moment one exists.
 */
export const PREVIEW_EMPTY_LOG_SEARCH_REPLY = /^No logs matching "[\s\S]*"\.$/u;

/** Splits one `preview_server_logs` reply into drawer lines. Empty for every "nothing yet" reply. */
export function previewLogLines(rendered: string): string[] {
  const text = rendered.replace(/\r\n/g, "\n");
  const reply = text.trim();
  if (
    !reply
    || PREVIEW_EMPTY_LOG_REPLIES.includes(reply)
    || PREVIEW_EMPTY_LOG_SEARCH_REPLY.test(reply)
  ) {
    return [];
  }
  const lines = text.split("\n");
  while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

export type PreviewLogSeverity = "error" | "warn";

/** Claude Code's own classifier: the first 200 characters, uppercased, decide the colour. */
export function previewLogSeverity(line: string): PreviewLogSeverity | null {
  const head = line.slice(0, 200).toUpperCase();
  if (
    head.includes("ERROR")
    || head.includes("ERR!")
    || head.includes("FATAL")
    || head.includes("FAIL")
  ) {
    return "error";
  }
  return head.includes("WARN") ? "warn" : null;
}

/** The source's log cadence. */
export const PREVIEW_LOG_POLL_INTERVAL_MS = 1000;

/** How often the pane re-reads the configuration file and the running-server list. */
export const PREVIEW_SERVER_POLL_INTERVAL_MS = 1500;
