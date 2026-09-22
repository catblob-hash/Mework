import { Channel, hasBackendRuntime, invoke } from "./backend";
import type { TerminalShell } from "./workspaces";

export interface TerminalCommandState {
  revision: number;
  status: "idle" | "running";
  commandId: string | null;
  commandCount: number;
}

export type TerminalEvent =
  | { type: "output"; sessionId: string; data: number[] }
  | { type: "command_state"; sessionId: string; commandState: TerminalCommandState }
  | { type: "ready"; sessionId: string }
  | { type: "exit"; sessionId: string; exitCode: number | null }
  | { type: "error"; sessionId: string; message: string };

export type TerminalPhase = "idle" | "connecting" | "running" | "closing" | "exited" | "error";

export interface TerminalSessionState {
  terminalId: string;
  conversationId: string;
  label: string;
  phase: TerminalPhase;
  busy: boolean;
  hasHistory: boolean;
  cwd: string;
  shell: string;
  sessionId: string | null;
}

export interface OpenTerminalResult {
  sessionId: string;
  created: boolean;
  running: boolean;
  ready: boolean;
  cwd: string;
  shell: string;
  snapshot: number[];
  commandState: TerminalCommandState;
}

function requireDesktopRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("终端仅可在桌面应用中使用");
}

/**
 * Where a terminal runs and what it runs: the conversation's workspace number —
 * the same 1-based address the model uses — and the shell. `null` leaves either
 * to the host: workspace 1, and that machine's default shell.
 */
export interface TerminalLaunchChoice {
  workspace: number | null;
  shell: TerminalShell | null;
}

export async function openTerminal(
  conversationId: string,
  terminalId: string,
  cols: number,
  rows: number,
  onEvent: (event: TerminalEvent) => void,
  launch: TerminalLaunchChoice = { workspace: null, shell: null }
): Promise<OpenTerminalResult> {
  requireDesktopRuntime();
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<OpenTerminalResult>("open_terminal", {
    conversationId,
    terminalId,
    cols,
    rows,
    workspace: launch.workspace,
    shell: launch.shell,
    onEvent: channel
  });
}

export async function writeTerminal(
  conversationId: string,
  terminalId: string,
  sessionId: string,
  data: string
): Promise<void> {
  requireDesktopRuntime();
  return invoke<void>("write_terminal", { conversationId, terminalId, sessionId, data });
}

export async function resizeTerminal(
  conversationId: string,
  terminalId: string,
  sessionId: string,
  cols: number,
  rows: number
): Promise<void> {
  requireDesktopRuntime();
  return invoke<void>("resize_terminal", { conversationId, terminalId, sessionId, cols, rows });
}

export async function detachTerminal(
  conversationId: string,
  terminalId: string,
  sessionId: string
): Promise<void> {
  requireDesktopRuntime();
  return invoke<void>("detach_terminal", { conversationId, terminalId, sessionId });
}

export async function closeTerminal(conversationId: string, terminalId: string): Promise<void> {
  requireDesktopRuntime();
  return invoke<void>("close_terminal", { conversationId, terminalId });
}
