import type { AttachedWorkspace, Conversation, RunTarget, SshMachineConfig, Workspace } from "../types";

export const TEMPORARY_WORKSPACE_ID = "__temporary__";

/**
 * Address key for a machine's environment-variable table. Matches the host
 * `run_environment::env_key` exactly.
 *
 * It doubles as the machine's identity wherever two workspaces have to be told
 * apart: one machine's `/srv/app` is not another's.
 */
export function runEnvKey(target: RunTarget | null | undefined): string {
  if (!target) return "local";
  return target.kind === "wsl" ? `wsl:${target.distro}` : `ssh:${target.machineId}`;
}

/** Whether two machine bindings name the same machine. */
export function sameMachine(
  left: RunTarget | null | undefined,
  right: RunTarget | null | undefined
): boolean {
  return runEnvKey(left) === runEnvKey(right);
}

/**
 * How a machine that is not this one is named beside a path: `WSL: Ubuntu`,
 * `SSH: devbox`. `null` for the host machine, which needs no qualifier.
 *
 * The SSH catalog name, not the id: a machine the user renamed is the one they
 * recognize by its name, and a dangling id is more honest shown as itself than
 * hidden behind a blank.
 */
export function workspaceMachineLabel(
  machine: RunTarget | null | undefined,
  sshMachines: readonly SshMachineConfig[]
): string | null {
  if (!machine) return null;
  if (machine.kind === "wsl") return `WSL: ${machine.distro}`;
  return `SSH: ${sshMachines.find((entry) => entry.id === machine.machineId)?.name ?? machine.machineId}`;
}

/** A path with its machine appended when the machine is not this one. */
export function workspaceLocationTitle(
  path: string,
  machine: RunTarget | null | undefined,
  sshMachines: readonly SshMachineConfig[]
): string {
  const label = workspaceMachineLabel(machine, sshMachines);
  return label ? `${path} (${label})` : path;
}

/**
 * The conversation's workspaces in the order the model addresses them: the
 * primary workspace is 1 and the attached ones follow.
 *
 * This mirrors `workspace_set::WorkspaceSet::resolve` on the host, which is the
 * authority — the renderer reads this only to label chips and to decide what the
 * tool picker may offer. The primary entry uses the worktree when the
 * conversation has one, because that is the directory its tools resolve against.
 */
export function conversationWorkspaces(
  workspace: Workspace | null | undefined,
  conversation: Pick<Conversation, "worktree" | "attachedWorkspaces"> | null | undefined
): AttachedWorkspace[] {
  const primary: AttachedWorkspace[] = workspace
    ? [{
      machine: conversation?.worktree ? null : workspace.machine ?? null,
      path: conversation?.worktree?.path ?? workspace.path
    }]
    : [];
  return [...primary, ...(conversation?.attachedWorkspaces ?? [])];
}

/**
 * Identifies the entry point that creates a conversation. The sidebar's New Task,
 * Ctrl+N, and empty-state button use `global` and always apply the global default
 * preset. A workspace title-row `+` uses `workspace`, first checking its default
 * preset, then its remembered conversation settings.
 */
export type NewConversationSource = "global" | "workspace";

export function isTemporaryWorkspace(workspace: Workspace | null | undefined): boolean {
  return workspace?.kind === "temporary" || workspace?.id === TEMPORARY_WORKSPACE_ID;
}

export function isReservedWorkspace(workspace: Workspace | null | undefined): boolean {
  return isTemporaryWorkspace(workspace);
}

export function createTemporaryWorkspace(conversations: Workspace["conversations"] = []): Workspace {
  return {
    id: TEMPORARY_WORKSPACE_ID,
    name: "临时工作区",
    kind: "temporary",
    path: "",
    createdAt: new Date().toISOString(),
    defaultConversationPresetId: "",
    lastConversationSettings: null,
    conversations
  };
}
