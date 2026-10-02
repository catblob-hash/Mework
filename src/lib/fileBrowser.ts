import { hasBackendRuntime, invoke } from "./backend";
import type { RunTarget, SshMachineConfig } from "../types";

/**
 * The file pane's view of the machines it browses and the files on them.
 *
 * Every path here is absolute on its machine and spelled the way the host's
 * file service spells it (`remote_agent::files`): `/` separated, a Windows
 * machine's with an upper-case drive (`C:/Users/dev`), where `/` alone is the
 * list of that machine's drives. The host normalizes; the renderer compares and
 * prints.
 */

/** A machine the pane can browse: this computer (`null`), a WSL distribution, a registered SSH machine. */
export type BrowseMachine = RunTarget | null;

export type BrowseEntryKind = "directory" | "file" | "other";

export interface BrowseEntry {
  name: string;
  /** The entry's own absolute path. */
  path: string;
  kind: BrowseEntryKind;
  /** A symbolic link; `kind` is what it leads to. */
  link: boolean;
  size: number | null;
}

export interface BrowseListing {
  path: string;
  parent: string | null;
  /** Whether the machine spells paths the Windows way. */
  windows: boolean;
  entries: BrowseEntry[];
  truncated: boolean;
}

export interface BrowseTextFile {
  path: string;
  content: string;
  truncated: boolean;
  size: number;
  binary: boolean;
}

export interface BrowseFileBytes {
  path: string;
  data: string;
  size: number;
  tooLarge: boolean;
}

export interface BrowseSearchMatch {
  name: string;
  /** Relative to the searched directory, `/` separated. */
  path: string;
  kind: "directory" | "file";
  positions: number[];
  score: number;
}

export interface BrowseSearchResults {
  query: string;
  root: string;
  matches: BrowseSearchMatch[];
  truncated: boolean;
}

export interface BrowseProbeTarget {
  machine: BrowseMachine;
  path: string;
}

export interface BrowseProbeResult {
  /** Normalized on its machine: `~` expanded, `..` applied. */
  path: string;
  kind: BrowseEntryKind | null;
  /** False when the machine could not be asked in time. */
  reached: boolean;
}

export interface OpenWithApp {
  id: string;
  name: string;
  icon: string | null;
  default: boolean;
}

export interface OpenWithChoices {
  apps: OpenWithApp[];
  /** Whether the system has a chooser of its own for everything else. */
  chooser: boolean;
}

/** Host-side cap; asking for more is clamped there. */
const SEARCH_LIMIT = 200;

function requireRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("文件浏览仅可在连接 Rust 后端时使用");
}

export async function browseListDirectory(machine: BrowseMachine, path: string): Promise<BrowseListing> {
  requireRuntime();
  return invoke<BrowseListing>("browse_list_directory", { machine, path });
}

export async function browseReadFile(machine: BrowseMachine, path: string): Promise<BrowseTextFile> {
  requireRuntime();
  return invoke<BrowseTextFile>("browse_read_file", { machine, path });
}

/** The whole file as base64, refused whole past the preview cap rather than cut. */
export async function browseReadFileBytes(machine: BrowseMachine, path: string): Promise<BrowseFileBytes> {
  requireRuntime();
  return invoke<BrowseFileBytes>("browse_read_file_bytes", { machine, path });
}

export async function browseSearchFiles(
  machine: BrowseMachine,
  root: string,
  query: string
): Promise<BrowseSearchResults> {
  requireRuntime();
  return invoke<BrowseSearchResults>("browse_search_files", { machine, root, query, limit: SEARCH_LIMIT });
}

/**
 * What is at each target; every machine is asked at once, each in one request.
 *
 * A probe is quick by default: a machine whose link is not up, or that does not
 * answer within a few seconds, comes back `reached: false`. `patient` waits for
 * the link the way a listing does, for a probe the reader asked for by name.
 */
export async function browseProbePaths(
  targets: readonly BrowseProbeTarget[],
  options: { patient?: boolean } = {}
): Promise<BrowseProbeResult[]> {
  requireRuntime();
  return invoke<BrowseProbeResult[]>("browse_probe_paths", { targets, patient: options.patient ?? false });
}

export async function browseRenamePath(machine: BrowseMachine, path: string, name: string): Promise<string> {
  requireRuntime();
  return (await invoke<{ path: string }>("browse_rename_path", { machine, path, name })).path;
}

/** To the Trash on this computer (`trashed`), for good on another machine. */
export async function browseDeletePath(machine: BrowseMachine, path: string): Promise<{ trashed: boolean }> {
  requireRuntime();
  return invoke<{ trashed: boolean }>("browse_delete_path", { machine, path });
}

export async function browseCreateDirectory(machine: BrowseMachine, parent: string, name: string): Promise<string> {
  requireRuntime();
  return (await invoke<{ path: string }>("browse_create_directory", { machine, parent, name })).path;
}

/** This computer only: a folder opened in the file manager, a file selected in its folder. */
export async function browseOpenInFileManager(path: string): Promise<void> {
  requireRuntime();
  await invoke<void>("browse_open_in_file_manager", { path });
}

export async function browseOpenWithChoices(path: string): Promise<OpenWithChoices> {
  requireRuntime();
  return invoke<OpenWithChoices>("browse_open_with_choices", { path });
}

/** Opens a file on this computer in a program the system offered for it, or its default with `null`. */
export async function browseOpenWith(path: string, app: string | null): Promise<void> {
  requireRuntime();
  await invoke<void>("browse_open_with", { path, app });
}

export async function browseOpenWithChooser(path: string): Promise<void> {
  requireRuntime();
  await invoke<void>("browse_open_with_chooser", { path });
}

// ---------------------------------------------------------------------------
// Machines
// ---------------------------------------------------------------------------

/** A machine's identity: `local`, `wsl:<distro>` or `ssh:<id>`, as the host keys it. */
export function machineKey(machine: BrowseMachine | undefined): string {
  if (!machine) return "local";
  return machine.kind === "wsl" ? `wsl:${machine.distro}` : `ssh:${machine.machineId}`;
}

export function sameBrowseMachine(left: BrowseMachine | undefined, right: BrowseMachine | undefined): boolean {
  return machineKey(left) === machineKey(right);
}

/** A file somewhere: the key every cache in the pane is kept under. */
export function locationKey(machine: BrowseMachine | undefined, path: string): string {
  return `${machineKey(machine)}\u0000${path}`;
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/** Whether `path` is spelled the Windows way: a drive, then a separator or nothing. */
export function isWindowsPath(path: string): boolean {
  return /^[A-Za-z]:([\\/]|$)/.test(path);
}

/**
 * A path in the host's spelling, for comparing one this renderer was handed —
 * a workspace root as the document records it, `C:\Projects\Mework`, or as the
 * host canonicalized it, `\\?\C:\Projects\Mework` — with one the file service
 * answered. Only the spelling changes: `~` and `..` stay for the host.
 */
export function browsePath(raw: string): string {
  let path = raw.trim();
  if (/^[\\/]{2}\?[\\/]/.test(path)) path = path.slice(4);
  if (isWindowsPath(path)) {
    path = path.replace(/\\/g, "/");
    path = `${path[0]!.toUpperCase()}${path.slice(1)}`;
    if (path.length === 2) path += "/";
  }
  path = path.replace(/(.)\/{2,}/g, "$1/");
  return path.length > 1 && !/^[A-Z]:\/$/.test(path) ? path.replace(/\/+$/, "") : path;
}

/** `dir` and `name` with one separator between them. */
export function joinBrowsePath(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

/**
 * Where "up" leads from `path`: `C:/` goes to the drives, `/` to the list of
 * machines (`null`).
 */
export function parentBrowsePath(path: string): string | null {
  if (path === "/" || path === "") return null;
  if (/^[A-Z]:\/?$/.test(path)) return "/";
  const trimmed = path.replace(/\/+$/, "");
  const cut = trimmed.lastIndexOf("/");
  if (cut < 0) return null;
  if (cut === 0) return "/";
  if (/^[A-Z]:$/.test(trimmed.slice(0, cut))) return `${trimmed.slice(0, cut)}/`;
  return trimmed.slice(0, cut);
}

/** The last segment: a file's name, a drive's letter, `/` for a root. */
export function browseName(path: string): string {
  if (/^[A-Z]:\/?$/.test(path)) return path.slice(0, 2);
  const trimmed = path.replace(/\/+$/, "");
  if (!trimmed) return "/";
  return trimmed.slice(trimmed.lastIndexOf("/") + 1);
}

/** Windows ignores case; everything else does not. */
function folded(path: string): string {
  return isWindowsPath(path) ? path.toLowerCase() : path;
}

/** `path` relative to `root`, `""` for the root itself, or null when it is elsewhere. */
export function relativeBrowsePath(root: string, path: string): string | null {
  const base = folded(root).replace(/\/+$/, "");
  const target = folded(path);
  if (target === base || target === `${base}/`) return "";
  const prefix = `${base}/`;
  if (!target.startsWith(prefix)) return null;
  return path.slice(prefix.length);
}

/** Every directory between `root` (exclusive) and `path` (exclusive), outermost first. */
export function browseAncestors(root: string, path: string): string[] {
  const relative = relativeBrowsePath(root, path);
  if (!relative) return [];
  const segments = relative.split("/");
  const ancestors: string[] = [];
  let current = root;
  for (const segment of segments.slice(0, -1)) {
    current = joinBrowsePath(current, segment);
    ancestors.push(current);
  }
  return ancestors;
}

/**
 * Resolves a path written against `base` (a directory, absolute on the same
 * machine). An absolute path stands alone; `..` past the top stays at the top.
 */
export function resolveBrowsePath(base: string, written: string): string {
  const raw = written.trim();
  const windows = isWindowsPath(base) || isWindowsPath(raw);
  const spelled = windows ? raw.replace(/\\/g, "/") : raw;
  if (spelled.startsWith("/") || isWindowsPath(spelled) || spelled.startsWith("~")) {
    return browsePath(spelled);
  }
  // A remote workspace may be recorded as `~/…`; its machine expands that.
  const home = base === "~" || base.startsWith("~/");
  const drive = /^[A-Z]:/i.exec(base)?.[0];
  const start = drive ? base.slice(2) : home ? base.slice(1) : base;
  const segments: string[] = [];
  for (const segment of `${start}/${spelled}`.split("/")) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") segments.pop();
    else segments.push(segment);
  }
  return `${drive ? drive.toUpperCase() : home ? "~" : ""}/${segments.join("/")}`;
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/** What the address bar needs to know about the machines it can name. */
export interface AddressContext {
  sshMachines: readonly SshMachineConfig[];
  /** Whether this computer is Windows, which decides how its paths are printed. */
  hostWindows: boolean;
}

/**
 * How a location is written in the address bar, machine included: a path on
 * this computer as the computer spells it, `devbox:/srv/app` for an SSH machine
 * (or `ssh://devbox:2222/srv/app` on a port of its own), and
 * `\\wsl.localhost\Ubuntu\home\dev` for a WSL distribution, which is how
 * Windows itself names one.
 */
export function formatAddress(machine: BrowseMachine, path: string, context: AddressContext): string {
  if (!machine) {
    return context.hostWindows && isWindowsPath(path) ? path.replace(/\//g, "\\") : path;
  }
  if (machine.kind === "wsl") {
    return `\\\\wsl.localhost\\${machine.distro}${path.replace(/\//g, "\\")}`;
  }
  const config = context.sshMachines.find((entry) => entry.id === machine.machineId);
  const host = config?.host ?? machine.machineId;
  const port = config?.port ?? 0;
  if (port && port !== 22) {
    return `ssh://${host}:${port}${path.startsWith("/") ? "" : "/"}${path}`;
  }
  return `${host}:${path}`;
}

/** What an address names: the list of machines, a place on one, or nothing it can read. */
export type ParsedAddress =
  | { kind: "machines" }
  | { kind: "location"; machine: BrowseMachine; path: string }
  | { kind: "error"; reason: "unknown-machine"; name: string };

/**
 * An SSH machine the user wrote as `host`: its catalog address (`user@host`,
 * `host`, an `~/.ssh/config` alias), the host part of that address, or its name.
 */
function sshMachineNamed(
  written: string,
  port: number | null,
  machines: readonly SshMachineConfig[]
): SshMachineConfig | null {
  const wanted = written.trim().toLowerCase();
  const portMatches = (machine: SshMachineConfig) => port === null || (machine.port || 22) === port;
  const byAddress = machines.find((machine) => portMatches(machine) && machine.host.toLowerCase() === wanted);
  if (byAddress) return byAddress;
  const byHost = machines.find((machine) => (
    portMatches(machine) && machine.host.toLowerCase().split("@").pop() === wanted.split("@").pop()
  ));
  if (byHost) return byHost;
  return machines.find((machine) => portMatches(machine) && machine.name.trim().toLowerCase() === wanted) ?? null;
}

/**
 * Reads what was typed into the address bar. A bare path is on this computer —
 * the address bar prints this computer's paths without a prefix, so that is how
 * they are typed back — and a relative one is taken against `current`, the
 * directory the bar was showing.
 */
export function parseAddress(
  typed: string,
  context: AddressContext,
  current: { machine: BrowseMachine; path: string } | null
): ParsedAddress {
  const text = typed.trim();
  if (!text) return { kind: "machines" };

  const wsl = /^(?:[\\/]{2}wsl(?:\.localhost|\$)[\\/]|wsl:\/\/)([^\\/]+)(.*)$/i.exec(text);
  if (wsl) {
    const rest = wsl[2]!.replace(/\\/g, "/");
    return { kind: "location", machine: { kind: "wsl", distro: wsl[1]! }, path: rest || "/" };
  }

  const url = /^ssh:\/\/([^/:]+(?:@[^/:]+)?)(?::(\d+))?(\/.*)?$/i.exec(text);
  if (url) {
    const machine = sshMachineNamed(url[1]!, url[2] ? Number(url[2]) : null, context.sshMachines);
    if (!machine) return { kind: "error", reason: "unknown-machine", name: url[1]! };
    // `ssh://host/~/notes` is the home-relative form scp and git write.
    const rest = url[3] ?? "/";
    const path = rest.startsWith("/~") ? rest.slice(1) : rest;
    return { kind: "location", machine: { kind: "ssh", machineId: machine.id }, path: path || "~" };
  }

  if (isWindowsPath(text)) {
    // A drive path on a page showing a Windows SSH machine stays on it.
    const machine = current?.machine && isWindowsPath(current.path) ? current.machine : null;
    return { kind: "location", machine, path: text };
  }

  // `host:path`, scp's form. A one-letter "host" is a drive, handled above.
  const scp = /^([^\s/\\:]{2,}|[^\s/\\:]+@[^\s/\\:]+):(.*)$/.exec(text);
  if (scp) {
    const machine = sshMachineNamed(scp[1]!, null, context.sshMachines);
    if (!machine) return { kind: "error", reason: "unknown-machine", name: scp[1]! };
    return { kind: "location", machine: { kind: "ssh", machineId: machine.id }, path: scp[2] || "~" };
  }

  if (text.startsWith("/") || text.startsWith("~") || (context.hostWindows && text.startsWith("\\"))) {
    return { kind: "location", machine: null, path: text };
  }
  if (current) {
    return { kind: "location", machine: current.machine, path: resolveBrowsePath(current.path, text) };
  }
  return { kind: "location", machine: null, path: text };
}

/** A machine's name in a menu or a tab: this computer, `WSL: Ubuntu`, an SSH machine's own name. */
export function browseMachineLabel(
  machine: BrowseMachine,
  sshMachines: readonly SshMachineConfig[],
  thisComputer: string
): string {
  if (!machine) return thisComputer;
  if (machine.kind === "wsl") return `WSL: ${machine.distro}`;
  return sshMachines.find((entry) => entry.id === machine.machineId)?.name ?? machine.machineId;
}
