import { Channel, hasBackendRuntime, invoke } from "./backend";

/** Mirror of Rust `shell_tasks::ShellTaskOutcome`. */
export type ShellTaskOutcome = "succeeded" | "failed" | "stopped";

/** Mirror of Rust `shell_tasks::ShellTaskSnapshot`. */
export interface ShellTaskSnapshot {
  shellTaskId: string;
  conversationId: string;
  /** `bash` or `powershell` — which tool is running. */
  toolName: string;
  /** Display-truncated command text, already collapsed to one line. */
  command: string;
  /** Set once a stop was asked for, before the process has actually gone. */
  stopping: boolean;
  /**
   * When the host registered the command, RFC3339. It is the host's clock and
   * not the renderer's on purpose: a command can start while nobody is
   * subscribed, and the reconcile that recovers it must still report how long it
   * has really been running rather than how long we have known about it.
   */
  startedAt: string;
  /**
   * When it ended, RFC3339, or null while it runs or when restart recovery
   * cannot establish the end time. `outcome`, not this field, decides liveness.
   */
  endedAt: string | null;
  /** How it ended, or null while it runs. A row is running exactly while this is null. */
  outcome: ShellTaskOutcome | null;
  /** Process exit code, when the command exited on its own and the platform reported one. */
  exitCode: number | null;
  /**
   * S11: true for the `run_in_background` leg. A background command is a
   * conversation task — it survives run settlement and run cancellation; only
   * its own stop button (or app exit) ends it early. Optional so older test
   * fixtures without the field stay valid; the host always sends it.
   */
  background?: boolean;
}

function requireDesktopRuntime(): void {
  if (!hasBackendRuntime()) throw new Error("Shell 命令仅可在桌面应用中使用");
}

/**
 * Asks one running command to stop. False means it had already finished, or it
 * belongs to another conversation — either way there is no process left to kill.
 */
export async function stopShellTask(
  conversationId: string,
  shellTaskId: string
): Promise<boolean> {
  requireDesktopRuntime();
  return invoke<boolean>("stop_shell_task", { conversationId, shellTaskId });
}

/**
 * Stops a conversation task that survives across turns (a child agent name,
 * `workflow:<runId>`, or `shell:<id>`). It does not stop the model run. The
 * stopped task settles like any other terminal result: it folds into the
 * timeline, wakes an idle conversation, and its body tells the model the user
 * closed it. False means the host has no live entry for that address.
 */
export async function stopConversationTask(
  conversationId: string,
  task: string
): Promise<boolean> {
  requireDesktopRuntime();
  return invoke<boolean>("stop_conversation_task", { conversationId, task });
}

/**
 * Every command this conversation has run, running and finished. Push events
 * carry the live changes; this recovers the set missed while there was no
 * subscription — a browser-dev reconnect, or arriving at a conversation mid-run.
 */
export async function listShellTasks(
  conversationId: string
): Promise<ShellTaskSnapshot[]> {
  requireDesktopRuntime();
  return invoke<ShellTaskSnapshot[]>("list_shell_tasks", { conversationId });
}

/** Which pipe a chunk came from. Mirror of Rust `shell_tasks::ShellOutputStream`. */
export type ShellOutputStream = "stdout" | "stderr";

/**
 * Live output of one command. Mirror of Rust `shell_tasks::ShellOutputEvent`.
 *
 * `text` is already decoded: the host reads the pipes in 8 KiB chunks and turns the bytes into
 * text itself — UTF-8, with the system ANSI code page as a per-line fallback for the GBK a
 * Windows PowerShell or a native program writes into a hidden console — so a multi-byte
 * character split by a chunk boundary is the host's problem, not this side's.
 */
export type ShellOutputEvent =
  | {
      type: "output";
      shellTaskId: string;
      stream: ShellOutputStream;
      /** Monotonic per command, assigned host-side so the two pipes cannot collide. */
      seq: number;
      text: string;
    }
  | {
      type: "end";
      shellTaskId: string;
      outcome: ShellTaskOutcome;
      exitCode: number | null;
    };

/** Mirror of Rust `shell_tasks::ShellTaskOutputHandle`. */
export interface ShellTaskOutputHandle {
  /** Everything the host still retains, oldest first, decoded the same way the events are. */
  snapshot: string;
  /** False for a command that already ended: the snapshot is all there will ever be. */
  live: boolean;
  /**
   * Bytes of decoded text the host's tail buffer dropped off the front. Non-zero means what
   * follows is a suffix — the model's copy of this output was truncated from the other end, so
   * the two genuinely differ.
   */
  droppedHeadBytes: number;
  /**
   * Names the sink this subscription installed. Hand it back to `detachShellTaskOutput` so a
   * detach that reaches the host late — after a newer subscription from the same page — removes
   * only its own sink. Zero for a command that had already ended.
   */
  subscriptionId: number;
}

/**
 * Starts watching one command's output. The host installs the channel and takes its retained
 * buffer in one step, so nothing printed between "read" and "subscribe" can go missing.
 */
export async function openShellTaskOutput(
  conversationId: string,
  shellTaskId: string,
  onEvent: (event: ShellOutputEvent) => void
): Promise<ShellTaskOutputHandle> {
  requireDesktopRuntime();
  const channel = new Channel<ShellOutputEvent>();
  channel.onmessage = onEvent;
  return invoke<ShellTaskOutputHandle>("open_shell_task_output", {
    conversationId,
    shellTaskId,
    onEvent: channel
  });
}

/**
 * Stops watching. False means the command is already gone, which is not an error. Without a
 * `subscriptionId` whatever sink is installed goes, which is only right for a caller that never
 * received a handle.
 */
export async function detachShellTaskOutput(
  conversationId: string,
  shellTaskId: string,
  subscriptionId?: number
): Promise<boolean> {
  requireDesktopRuntime();
  return invoke<boolean>("detach_shell_task_output", { conversationId, shellTaskId, subscriptionId });
}
