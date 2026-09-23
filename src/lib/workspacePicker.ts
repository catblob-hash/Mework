import type { RunTarget } from "../types";
import { hasBackendRuntime, invoke } from "./backend";

export function hasNativeWorkspacePicker(): boolean {
  return hasBackendRuntime();
}

export async function pickWorkspaceDirectory(): Promise<string | null> {
  if (!hasNativeWorkspacePicker()) return null;
  return invoke<string | null>("pick_workspace_directory");
}

/** One subdirectory in a remote listing. */
export interface RemoteDirectoryEntry {
  name: string;
  /**
   * The path to ask for when the user opens it, spelled by the host: only it
   * knows the machine's path rules (`/srv` + `app`, or the drive list's `C:`
   * opening as `C:/`).
   */
  path: string;
}

/** One level of a machine's filesystem, as the remote browser reads it. */
export interface RemoteDirectoryListing {
  /**
   * The directory the host actually resolved, with `~` expanded and `..`
   * applied. The browser shows this rather than what it asked for, because the
   * remote shell is the only thing that can say where a path really leads. A
   * Windows machine's paths come with forward slashes: `C:/Users/dev`.
   */
  path: string;
  /** The directory to go up to, or `null` at the top. */
  parent: string | null;
  /** Immediate subdirectories, sorted. */
  entries: RemoteDirectoryEntry[];
}

/**
 * Lists one directory on a WSL or SSH machine.
 *
 * There is no native folder dialog for a machine that is not this one, so the
 * host reads the directory through that machine's own login shell — whichever
 * shell it is: a Unix machine is read with `/bin/sh`, a Windows machine with
 * PowerShell. An unreachable machine or an unreadable directory comes back as
 * an error string, not as an empty listing that would read as "this directory
 * is empty".
 */
export async function listRemoteDirectory(
  machine: RunTarget,
  path: string
): Promise<RemoteDirectoryListing> {
  return invoke<RemoteDirectoryListing>("list_remote_directory", { machine, path });
}

/**
 * Records a remote directory as granted to this session and returns the path
 * the host resolved.
 *
 * The browser is what authorizes a remote workspace, exactly as the native
 * dialog authorizes a local one: the host refuses to save a document naming a
 * workspace no picker of its own ever returned, so a path typed into the
 * document by the renderer cannot widen what a conversation can reach.
 */
export async function authorizeRemoteWorkspace(
  machine: RunTarget,
  path: string
): Promise<string> {
  return invoke<string>("authorize_remote_workspace", { machine, path });
}
