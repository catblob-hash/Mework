import {
  ChevronDown,
  Folder,
  FolderClock,
  FolderKanban,
  FolderPlus,
  Server,
  SquareTerminal
} from "lucide-react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { AttachedWorkspace, RunTarget, SshMachineConfig, Workspace } from "../types";
import {
  isTemporaryWorkspace,
  projectWorkspaces,
  TEMPORARY_WORKSPACE_ID,
  terminalShellLabel,
  workspaceDirectoryLabel,
  workspaceLocationTitle
} from "../lib/workspaces";
import type { TerminalShell } from "../lib/workspaces";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem } from "./PopoverMenu";

/** The icon that says which machine a directory is on: this one, a WSL distribution, an SSH machine. */
export function machineIcon(machine: RunTarget | null | undefined, size: number): ReactNode {
  if (!machine) return <Folder size={size} />;
  return machine.kind === "wsl" ? <SquareTerminal size={size} /> : <Server size={size} />;
}

/** Every directory of a project, one per line, each with its machine when that is not this one. */
function projectTitle(project: Workspace, sshMachines: readonly SshMachineConfig[]): string {
  return projectWorkspaces(project)
    .map((workspace) => workspaceLocationTitle(workspace.path, workspace.machine, sshMachines))
    .join("\n") || project.name;
}

/**
 * The composer's project chip: which project a new task belongs to.
 *
 * Choosing one moves the task there. It is only offered before the task starts — once a
 * conversation has content its project is settled, and the caller stops rendering the chip.
 */
export function ProjectSelector({
  projects,
  activeProject,
  sshMachines,
  disabled = false,
  disabledReason,
  isProjectDeleting = () => false,
  onSelect,
  onCreateProject
}: {
  projects: Workspace[];
  /** `null` means no project is chosen yet; sending then uses the temporary project. */
  activeProject: Workspace | null;
  sshMachines: readonly SshMachineConfig[];
  disabled?: boolean;
  /** Takes the chip's title while it is disabled, so the reason is reachable. */
  disabledReason?: string;
  isProjectDeleting?: (projectId: string) => boolean;
  onSelect: (projectId: string) => void;
  onCreateProject: () => void;
}) {
  const { t } = useI18n();
  const available = projects.filter((project) => project.kind === "directory");
  const activeName = !activeProject
    ? t("选择项目", "Choose project")
    : isTemporaryWorkspace(activeProject)
      ? t("临时项目", "Temporary project")
      : activeProject.name;
  const sourceDeleting = disabled || Boolean(activeProject && isProjectDeleting(activeProject.id));

  const choose = (projectId: string) => {
    if (sourceDeleting || isProjectDeleting(projectId)) return;
    onSelect(projectId);
  };

  return (
    <PopoverMenu
      rootClassName="project-selector"
      triggerClassName="composer-chip"
      trigger={<>
        {isTemporaryWorkspace(activeProject) ? <FolderClock size={13} /> : <FolderKanban size={13} />}
        <span className="composer-chip__label">{activeName}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </>}
      triggerLabel={t("项目：{name}", "Project: {name}", { name: activeName })}
      triggerTitle={sourceDeleting && disabledReason
        ? disabledReason
        : activeProject && !isTemporaryWorkspace(activeProject)
          ? projectTitle(activeProject, sshMachines)
          : activeName}
      disabled={sourceDeleting}
      menuLabel={t("选择项目", "Select project")}
      menuWidth={252}
      sections={[
        {
          id: "projects",
          label: t("项目", "Projects"),
          items: available.length
            ? available.map((project) => ({
              id: project.id,
              label: project.name,
              title: projectTitle(project, sshMachines),
              icon: <FolderKanban size={14} />,
              hint: project.additionalWorkspaces?.length
                ? String(project.additionalWorkspaces.length + 1)
                : undefined,
              checked: project.id === activeProject?.id,
              disabled: sourceDeleting || isProjectDeleting(project.id),
              onSelect: () => choose(project.id)
            }))
            : [{
              id: "empty",
              label: t("还没有项目", "No projects yet"),
              icon: <Folder size={14} />,
              disabled: true
            }]
        },
        {
          id: "actions",
          items: [
            {
              id: TEMPORARY_WORKSPACE_ID,
              label: t("临时项目", "Temporary project"),
              icon: <FolderClock size={14} />,
              checked: isTemporaryWorkspace(activeProject),
              disabled: sourceDeleting || isProjectDeleting(TEMPORARY_WORKSPACE_ID),
              onSelect: () => choose(TEMPORARY_WORKSPACE_ID)
            },
            {
              id: "create",
              label: t("新建项目…", "New project…"),
              icon: <FolderPlus size={14} />,
              disabled: sourceDeleting,
              onSelect: () => onCreateProject()
            }
          ]
        }
      ]}
    />
  );
}

/**
 * The composer's workspace chip, shown when the project has more than one workspace.
 *
 * Choosing one changes nothing in the conversation — every workspace of the project stays
 * reachable by its number. It only decides which directory the Git chip beside it describes
 * and where the terminal button opens a shell.
 */
export function WorkspaceMemberSelector({
  workspaces,
  selected,
  sshMachines,
  onSelect
}: {
  /** The project's workspaces in order; the first is the project's own directory. */
  workspaces: readonly AttachedWorkspace[];
  /** 1-based. */
  selected: number;
  sshMachines: readonly SshMachineConfig[];
  onSelect: (member: number) => void;
}) {
  const { t } = useI18n();
  const current = workspaces[selected - 1] ?? workspaces[0];
  if (!current) return null;
  const label = workspaceDirectoryLabel(current.path);
  return (
    <PopoverMenu
      rootClassName="workspace-member-selector"
      triggerClassName="composer-chip"
      trigger={<>
        {machineIcon(current.machine, 13)}
        <span className="composer-chip__index" aria-hidden="true">{selected}</span>
        <span className="composer-chip__label">{label}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </>}
      triggerLabel={t("工作区：{name}", "Workspace: {name}", { name: label })}
      triggerTitle={workspaceLocationTitle(current.path, current.machine, sshMachines)}
      menuLabel={t("选择工作区", "Select workspace")}
      menuWidth={260}
      sections={[{
        id: "workspaces",
        label: t("查看哪个工作区的 Git", "Show Git for"),
        items: workspaces.map((workspace, index): PopoverMenuItem => ({
          id: `${index + 1}`,
          label: workspaceDirectoryLabel(workspace.path),
          title: workspaceLocationTitle(workspace.path, workspace.machine, sshMachines),
          icon: machineIcon(workspace.machine, 14),
          hint: String(index + 1),
          checked: index + 1 === selected,
          onSelect: () => onSelect(index + 1)
        }))
      }]}
    />
  );
}

/** The menu of shells a terminal can start with, for a workspace on one machine. */
export function terminalShellMenuItems(
  shells: readonly TerminalShell[],
  onSelect: (shell: TerminalShell) => void,
  disabled = false
): PopoverMenuItem[] {
  return shells.map((shell) => ({
    id: shell,
    label: terminalShellLabel(shell),
    icon: <SquareTerminal size={14} />,
    disabled,
    onSelect: () => onSelect(shell)
  }));
}

/**
 * The composer's terminal button: opens a new terminal in the selected workspace, in the shell
 * picked from its menu. Which shells are offered follows that workspace's machine.
 */
export function TerminalShellButton({
  workspaceLabel,
  shells,
  disabled = false,
  disabledReason,
  onSelect
}: {
  /** The directory the shell will start in, for the button's name. */
  workspaceLabel: string;
  shells: readonly TerminalShell[];
  disabled?: boolean;
  disabledReason?: string;
  onSelect: (shell: TerminalShell) => void;
}) {
  const { t } = useI18n();
  const label = t("在 {name} 打开终端", "Open a terminal in {name}", { name: workspaceLabel });
  return (
    <PopoverMenu
      rootClassName="terminal-shell-button"
      triggerClassName="composer-chip composer-chip--icon"
      trigger={<SquareTerminal size={13} />}
      triggerLabel={label}
      triggerTitle={disabled && disabledReason ? disabledReason : label}
      disabled={disabled}
      menuLabel={t("用哪个 shell", "Which shell")}
      menuWidth={200}
      dense
      sections={[{ id: "shells", items: terminalShellMenuItems(shells, onSelect) }]}
    />
  );
}
