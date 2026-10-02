import {
  ChevronDown,
  Folder,
  FolderClock,
  FolderKanban,
  Server,
  Settings,
  SquareTerminal
} from "lucide-react";
import type { ReactNode } from "react";
import { useI18n } from "../i18n";
import type { AttachedWorkspace, RunTarget, SshMachineConfig, Workspace } from "../types";
import {
  isTemporaryWorkspace,
  projectWorkspaces,
  runEnvKey,
  TEMPORARY_WORKSPACE_ID,
  terminalShellLabel,
  workspaceDirectoryLabel,
  workspaceLocationTitle,
  workspaceMachineLabel
} from "../lib/workspaces";
import type { TerminalShell } from "../lib/workspaces";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem, PopoverMenuSection } from "./PopoverMenu";

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
      placement="above"
      sections={[
        {
          id: "projects",
          label: t("项目", "Projects"),
          items: available.length
            ? available.map((project) => ({
              id: project.id,
              label: project.name,
              title: projectTitle(project, sshMachines),
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
              disabled: true
            }]
        },
        {
          id: "actions",
          items: [
            {
              id: TEMPORARY_WORKSPACE_ID,
              label: t("临时项目", "Temporary project"),
              checked: isTemporaryWorkspace(activeProject),
              disabled: sourceDeleting || isProjectDeleting(TEMPORARY_WORKSPACE_ID),
              onSelect: () => choose(TEMPORARY_WORKSPACE_ID)
            },
            {
              id: "create",
              label: t("新建项目…", "New project…"),
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
 * The composer's workspace chip: the project's workspaces, grouped by the machine each is on.
 *
 * Choosing one changes nothing in the conversation — every workspace of the project stays
 * reachable by its number. It only decides which directory the Git chip beside it describes
 * and where the terminal pane starts a shell when it opens with none.
 *
 * Each machine heading carries a gear for that machine's settings, and each workspace a gear
 * for its own — its sandbox and variables, which belong to the workspace, not to its machine.
 */
export function WorkspaceMemberSelector({
  workspaces,
  selected,
  sshMachines,
  showIndex = true,
  onSelect,
  onConfigureMachine,
  onConfigureWorkspace
}: {
  /** The project's workspaces in order; the first is the project's own directory. */
  workspaces: readonly AttachedWorkspace[];
  /** 1-based. */
  selected: number;
  sshMachines: readonly SshMachineConfig[];
  /**
   * Whether the chip shows the selected workspace's number. Numbers are addresses the host
   * states to the model only when the conversation has more than one workspace; a lone
   * workspace's "1" would name something the model never sees.
   */
  showIndex?: boolean;
  onSelect: (member: number) => void;
  /** Opens the settings of a machine (`null` is this one). */
  onConfigureMachine: (machine: RunTarget | null) => void;
  /** Opens the settings of workspace `member` (1-based): its sandbox and variables. */
  onConfigureWorkspace: (member: number) => void;
}) {
  const { t } = useI18n();
  const current = workspaces[selected - 1] ?? workspaces[0];
  if (!current) return null;
  const label = workspaceDirectoryLabel(current.path);
  // One section per machine, in the order the machines first appear, so the numbers
  // still read in ascending order within each group.
  const groups = new Map<string, { machine: RunTarget | null; items: PopoverMenuItem[] }>();
  workspaces.forEach((workspace, index) => {
    const machine = workspace.machine ?? null;
    const key = runEnvKey(machine);
    const directory = workspaceDirectoryLabel(workspace.path);
    const group = groups.get(key) ?? { machine, items: [] };
    group.items.push({
      id: `${index + 1}`,
      label: directory,
      title: workspaceLocationTitle(workspace.path, workspace.machine, sshMachines),
      hint: String(index + 1),
      checked: index + 1 === selected,
      onSelect: () => onSelect(index + 1),
      action: {
        label: t("{name} 的设置", "Settings for {name}", { name: directory }),
        icon: <Settings size={13} />,
        onSelect: () => onConfigureWorkspace(index + 1)
      }
    });
    groups.set(key, group);
  });
  const sections = Array.from(groups, ([key, group]): PopoverMenuSection => {
    const machineName = workspaceMachineLabel(group.machine, sshMachines) ?? t("本机", "This machine");
    return {
      id: key,
      label: machineName,
      action: {
        label: t("{name} 的设置", "Settings for {name}", { name: machineName }),
        icon: <Settings size={13} />,
        onSelect: () => onConfigureMachine(group.machine)
      },
      items: group.items
    };
  });
  return (
    <PopoverMenu
      rootClassName="workspace-member-selector"
      triggerClassName="composer-chip"
      trigger={<>
        {machineIcon(current.machine, 13)}
        {showIndex && <span className="composer-chip__index" aria-hidden="true">{selected}</span>}
        <span className="composer-chip__label">{label}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </>}
      triggerLabel={t("工作区：{name}", "Workspace: {name}", { name: label })}
      triggerTitle={workspaceLocationTitle(current.path, current.machine, sshMachines)}
      menuLabel={t("选择工作区", "Select workspace")}
      menuWidth={260}
      placement="above"
      sections={sections}
    />
  );
}

/**
 * The menu of shells a terminal can start with, for a workspace on one machine: the ones its
 * probe found, most preferred first. A machine with none gets a row that says so, so a menu
 * never opens empty.
 */
export function terminalShellMenuItems(
  shells: readonly TerminalShell[],
  onSelect: (shell: TerminalShell) => void,
  emptyLabel: string,
  disabled = false
): PopoverMenuItem[] {
  if (!shells.length) return [{ id: "none", label: emptyLabel, icon: <SquareTerminal size={14} />, disabled: true }];
  return shells.map((shell) => ({
    id: shell,
    label: terminalShellLabel(shell),
    icon: <SquareTerminal size={14} />,
    disabled,
    onSelect: () => onSelect(shell)
  }));
}

/**
 * The conversation's workspaces as a menu, each opening its machine's shells beside it — the
 * top-right terminal button's menu once there is more than one place a terminal could start.
 * `member` is the conversation's 1-based workspace number, the address a terminal is opened by.
 */
export function terminalWorkspaceMenuItems({
  workspaces,
  sshMachines,
  shellsFor,
  emptyLabel,
  disabled = false,
  onSelect
}: {
  workspaces: readonly AttachedWorkspace[];
  sshMachines: readonly SshMachineConfig[];
  shellsFor: (machine: RunTarget | null) => readonly TerminalShell[];
  emptyLabel: string;
  disabled?: boolean;
  onSelect: (member: number, shell: TerminalShell) => void;
}): PopoverMenuItem[] {
  return workspaces.map((workspace, index) => ({
    id: `workspace-${index + 1}`,
    label: workspaceDirectoryLabel(workspace.path),
    title: workspaceLocationTitle(workspace.path, workspace.machine, sshMachines),
    icon: machineIcon(workspace.machine, 14),
    hint: String(index + 1),
    disabled,
    children: terminalShellMenuItems(
      shellsFor(workspace.machine ?? null),
      (shell) => onSelect(index + 1, shell),
      emptyLabel
    )
  }));
}

/**
 * The conversation's workspaces as a menu of places a preview page can be opened for — the top
 * bar's preview button and the preview pane's `+` once there is more than one. A page belongs to
 * the workspace it was opened for: its start page lists that workspace's `.mework/launch.json`,
 * and a server it runs runs on that workspace's machine.
 */
export function previewWorkspaceMenuItems({
  workspaces,
  sshMachines,
  disabled = false,
  onSelect
}: {
  workspaces: readonly AttachedWorkspace[];
  sshMachines: readonly SshMachineConfig[];
  disabled?: boolean;
  onSelect: (member: number) => void;
}): PopoverMenuItem[] {
  return workspaces.map((workspace, index) => ({
    id: `workspace-${index + 1}`,
    label: workspaceDirectoryLabel(workspace.path),
    title: workspaceLocationTitle(workspace.path, workspace.machine, sshMachines),
    icon: machineIcon(workspace.machine, 14),
    hint: String(index + 1),
    disabled,
    onSelect: () => onSelect(index + 1)
  }));
}
