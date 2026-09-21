import { ChevronDown, Folder, FolderClock, FolderOpen, Monitor, Server, SquareTerminal } from "lucide-react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { RunTarget, SshMachineConfig, Workspace, WslDistro } from "../types";
import {
  isTemporaryWorkspace,
  TEMPORARY_WORKSPACE_ID,
  workspaceLocationTitle
} from "../lib/workspaces";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem } from "./PopoverMenu";

/** The machines a directory can be chosen on, besides this one. */
export interface WorkspaceMachines {
  /** `null` until the menu has been opened and the distributions enumerated. */
  wslDistros: WslDistro[] | null;
  sshMachines: SshMachineConfig[];
}

/**
 * Workspace chip at the top of the composer.
 *
 * Switching workspaces moves the current conversation. Once it has content, the
 * caller makes the chip read-only instead of hiding it, because it still identifies
 * the conversation's workspace.
 *
 * A workspace lives on a machine — this one, a WSL distribution, or an SSH
 * machine — so "choose a workspace" first asks which machine to browse. The
 * host machine has a native folder dialog; the others are browsed through their
 * own shell, and the caller opens that browser.
 */
export function WorkspaceSelector({
  workspaces,
  activeWorkspace,
  machines,
  remoteMachines = true,
  onOpen,
  onSelect,
  onChooseDirectory,
  movementDisabled = false,
  movementDisabledReason,
  isWorkspaceDeleting = () => false
}: {
  workspaces: Workspace[];
  /** `null` means no workspace is selected. A draft may remain here until send assigns a temporary workspace. */
  activeWorkspace: Workspace | null;
  machines: WorkspaceMachines;
  /**
   * Whether directories on other machines may be chosen at all. Without a host
   * picker there is only the manual path dialog, which is for this machine.
   */
  remoteMachines?: boolean;
  /** Fires when the menu opens, which is when the WSL distributions are worth enumerating. */
  onOpen?: () => void;
  onSelect: (workspaceId: string) => void;
  /** `null` is this machine. */
  onChooseDirectory: (machine: RunTarget | null) => void;
  movementDisabled?: boolean;
  /** Explains why switching is disabled in the chip title when present. */
  movementDisabledReason?: string;
  isWorkspaceDeleting?: (workspaceId: string) => boolean;
}) {
  const { t } = useI18n();
  const available = workspaces.filter((workspace) => workspace.kind === "directory");
  const activeWorkspaceName = !activeWorkspace
    ? t("选择工作区", "Choose workspace")
    : isTemporaryWorkspace(activeWorkspace)
      ? t("临时工作区", "Temporary workspace")
      : activeWorkspace.name;
  const sourceDeleting = movementDisabled
    || Boolean(activeWorkspace && isWorkspaceDeleting(activeWorkspace.id));

  const choose = (workspaceId: string) => {
    if (
      movementDisabled
      || (activeWorkspace && isWorkspaceDeleting(activeWorkspace.id))
      || isWorkspaceDeleting(workspaceId)
    ) return;
    onSelect(workspaceId);
  };

  const chooseOn = (machine: RunTarget | null) => {
    if (sourceDeleting) return;
    onChooseDirectory(machine);
  };

  const machineChildren: PopoverMenuItem[] = [
    {
      id: "choose:local",
      label: t("本机", "This machine"),
      icon: <Monitor size={14} />,
      disabled: sourceDeleting,
      onSelect: () => chooseOn(null)
    },
    ...(machines.wslDistros ?? []).map((distro): PopoverMenuItem => ({
      id: `choose:wsl:${distro.name}`,
      label: distro.name,
      icon: <SquareTerminal size={14} />,
      disabled: sourceDeleting,
      onSelect: () => chooseOn({ kind: "wsl", distro: distro.name })
    })),
    ...machines.sshMachines.map((machine): PopoverMenuItem => ({
      id: `choose:ssh:${machine.id}`,
      label: machine.name,
      icon: <Server size={14} />,
      disabled: sourceDeleting,
      onSelect: () => chooseOn({ kind: "ssh", machineId: machine.id })
    }))
  ];

  return (
    <PopoverMenu
      rootClassName="workspace-selector"
      triggerClassName="composer-chip"
      trigger={<>
        {isTemporaryWorkspace(activeWorkspace)
          ? <FolderClock size={13} />
          : machineIcon(activeWorkspace?.machine, 13)}
        <span className="composer-chip__label">{activeWorkspaceName}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </>}
      triggerLabel={t("工作区：{name}", "Workspace: {name}", { name: activeWorkspaceName })}
      triggerTitle={sourceDeleting && movementDisabledReason
        ? movementDisabledReason
        : activeWorkspace?.path
          ? workspaceLocationTitle(activeWorkspace.path, activeWorkspace.machine, machines.sshMachines)
          : activeWorkspaceName}
      disabled={sourceDeleting}
      menuLabel={t("选择工作区", "Select workspace")}
      menuWidth={252}
      onOpen={onOpen}
      sections={[
        {
          id: "recent",
          label: t("最近", "Recent"),
          items: available.length
            ? available.map((workspace) => ({
              id: workspace.id,
              label: workspace.name,
              title: workspace.path
                ? workspaceLocationTitle(workspace.path, workspace.machine, machines.sshMachines)
                : workspace.name,
              icon: machineIcon(workspace.machine, 14),
              checked: workspace.id === activeWorkspace?.id,
              disabled: sourceDeleting || isWorkspaceDeleting(workspace.id),
              onSelect: () => choose(workspace.id)
            }))
            : [{
              id: "empty",
              label: t("还没有工作区", "No workspaces yet"),
              icon: <Folder size={14} />,
              disabled: true
            }]
        },
        {
          id: "actions",
          items: [
            remoteMachines
              ? {
                id: "choose",
                label: t("选择工作区", "Choose workspace"),
                icon: <FolderOpen size={14} />,
                disabled: sourceDeleting,
                children: machineChildren
              }
              : {
                id: "choose",
                label: t("选择工作区", "Choose workspace"),
                icon: <FolderOpen size={14} />,
                disabled: sourceDeleting,
                onSelect: () => chooseOn(null)
              },
            {
              id: TEMPORARY_WORKSPACE_ID,
              label: t("临时工作区", "Temporary workspace"),
              icon: <FolderClock size={14} />,
              checked: isTemporaryWorkspace(activeWorkspace),
              disabled: sourceDeleting || isWorkspaceDeleting(TEMPORARY_WORKSPACE_ID),
              onSelect: () => choose(TEMPORARY_WORKSPACE_ID)
            }
          ]
        }
      ]}
    />
  );
}

/**
 * The icon that says which machine a workspace is on. Machine and directory
 * are one fact on a chip this small: the icon carries the machine, the label
 * the directory, and the pair is spelled out in the title.
 */
function machineIcon(machine: RunTarget | null | undefined, size: number): ReactNode {
  if (!machine) return <Folder size={size} />;
  return machine.kind === "wsl" ? <SquareTerminal size={size} /> : <Server size={size} />;
}
