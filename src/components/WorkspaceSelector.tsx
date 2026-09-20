import { ChevronDown, Folder, FolderClock, FolderOpen } from "lucide-react";
import { useI18n } from "../i18n";
import type { Workspace } from "../types";
import { isTemporaryWorkspace, TEMPORARY_WORKSPACE_ID } from "../lib/workspaces";
import { PopoverMenu } from "./PopoverMenu";

/**
 * Workspace chip at the top of the composer.
 *
 * Switching workspaces moves the current conversation. Once it has content, the
 * caller makes the chip read-only instead of hiding it, because it still identifies
 * the conversation's workspace.
 */
export function WorkspaceSelector({
  workspaces,
  activeWorkspace,
  onSelect,
  onChooseDirectory,
  movementDisabled = false,
  movementDisabledReason,
  isWorkspaceDeleting = () => false
}: {
  workspaces: Workspace[];
  /** `null` means no workspace is selected. A draft may remain here until send assigns a temporary workspace. */
  activeWorkspace: Workspace | null;
  onSelect: (workspaceId: string) => void;
  onChooseDirectory: () => void;
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

  return (
    <PopoverMenu
      rootClassName="workspace-selector"
      triggerClassName="composer-chip"
      trigger={<>
        {isTemporaryWorkspace(activeWorkspace) ? <FolderClock size={13} /> : <Folder size={13} />}
        <span className="composer-chip__label">{activeWorkspaceName}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </>}
      triggerLabel={t("工作区：{name}", "Workspace: {name}", { name: activeWorkspaceName })}
      triggerTitle={sourceDeleting && movementDisabledReason
        ? movementDisabledReason
        : activeWorkspace?.path || activeWorkspaceName}
      disabled={sourceDeleting}
      menuLabel={t("选择工作区", "Select workspace")}
      menuWidth={252}
      sections={[
        {
          id: "recent",
          label: t("最近", "Recent"),
          items: available.length
            ? available.map((workspace) => ({
              id: workspace.id,
              label: workspace.name,
              title: workspace.path || workspace.name,
              icon: <Folder size={14} />,
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
            {
              id: "choose",
              label: t("选择工作区", "Choose workspace"),
              icon: <FolderOpen size={14} />,
              disabled: sourceDeleting,
              onSelect: () => {
                if (sourceDeleting) return;
                onChooseDirectory();
              }
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
