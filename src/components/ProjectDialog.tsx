import {
  ChevronDown,
  Folder,
  FolderPlus,
  LoaderCircle,
  Monitor,
  Plus,
  Server,
  Settings,
  SquareTerminal,
  X
} from "lucide-react";
import { useCallback, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { errorMessage } from "../lib/errors";
import { listWslDistros, MAX_PROJECT_WORKSPACES } from "../lib/runtime";
import { runEnvKey } from "../lib/workspaces";
import type { MachineUsage } from "../lib/workspaces";
import type { AttachedWorkspace, RunTarget, SshMachineConfig, WslDistro } from "../types";
import { Dialog, IconButton } from "./Common";
import { MachineSettingsDialog, SshMachineDialog, type MachineShellsControl } from "./MachineDialogs";
import { PathText } from "./PathText";
import { PopoverMenu } from "./PopoverMenu";
import type { PopoverMenuItem, PopoverPanelRect } from "./PopoverMenu";
import { RemoteDirectoryPicker } from "./RemoteDirectoryPicker";
import "./ProjectDialog.css";

export interface ProjectDialogProps {
  mode: "create" | "edit";
  /** The project's display name; blank means the first workspace's folder name. */
  initialName?: string;
  /**
   * The project's workspaces, the first being the primary. In edit mode the
   * first is locked: it is the project's identity — the file pane, its memory
   * and capability files, and the worktree records written before every
   * workspace could have one all hang off it.
   */
  initialWorkspaces?: AttachedWorkspace[];
  sshMachines: SshMachineConfig[];
  /** Whether the machine menu offers WSL distributions at all. */
  showWsl: boolean;
  /** Whether the host has a native folder dialog; without one a local row takes a typed path. */
  nativePicker: boolean;
  /** Opens the host's folder dialog. Resolves `null` when the user cancels. */
  onPickLocalDirectory: () => Promise<string | null>;
  /** Registers a machine created from a row's machine menu, or saves one edited from its gear. */
  onSaveSshMachine: (machine: SshMachineConfig) => void;
  /** Deletes an SSH machine from the catalog, from the settings its gear opens. */
  onDeleteSshMachine: (machineId: string) => void;
  /** What has a workspace on a machine (`null` is this one), stated in its settings. */
  machineUsage: (machine: RunTarget | null) => MachineUsage;
  /** Every machine's shells, shown and probed from its settings. */
  machineShells: MachineShellsControl;
  /** `workspaces[0]` is the primary; the name is trimmed and may be empty. */
  onSubmit: (name: string, workspaces: AttachedWorkspace[]) => void;
  onClose: () => void;
}

interface ProjectRow {
  /** Stable React key; rows are reordered by removal. */
  id: string;
  machine: RunTarget | null;
  path: string;
  /** The primary of a project being edited: its machine and path cannot change. */
  locked: boolean;
}

/** A drive-letter or UNC path: Windows compares those without regard to case. */
function looksLikeWindowsPath(path: string): boolean {
  return /^[A-Za-z]:([\\/]|$)/.test(path) || path.startsWith("\\\\");
}

/** Identity of a row for duplicate detection: the machine, then the path as that machine compares it. */
export function projectWorkspaceKey(machine: RunTarget | null | undefined, path: string): string {
  const trimmed = path.trim();
  return `${runEnvKey(machine)} ${looksLikeWindowsPath(trimmed) ? trimmed.toLowerCase() : trimmed}`;
}

function MachineIcon({ machine, size }: { machine: RunTarget | null; size: number }) {
  if (!machine) return <Monitor size={size} />;
  return machine.kind === "wsl" ? <SquareTerminal size={size} /> : <Server size={size} />;
}

/**
 * Creates or edits a project: a display name and one or more workspaces, each a
 * directory on a machine — this one, a WSL distribution, or an SSH machine.
 *
 * Every path comes back from a picker that authorizes it — the host's folder
 * dialog here, the remote browser elsewhere — because the host refuses to save
 * a project naming a directory no picker returned. Only the browser preview,
 * which has no host, takes a typed path.
 */
export function ProjectDialog({
  mode,
  initialName = "",
  initialWorkspaces,
  sshMachines,
  showWsl,
  nativePicker,
  onPickLocalDirectory,
  onSaveSshMachine,
  onDeleteSshMachine,
  machineUsage,
  machineShells,
  onSubmit,
  onClose
}: ProjectDialogProps) {
  const { t } = useI18n();
  const nextRowId = useRef(0);
  const newRowId = () => {
    nextRowId.current += 1;
    return `row-${nextRowId.current}`;
  };
  const [name, setName] = useState(initialName);
  const [rows, setRows] = useState<ProjectRow[]>(() => {
    const initial = initialWorkspaces?.length
      ? initialWorkspaces.map((workspace, index): ProjectRow => ({
        id: `initial-${index}`,
        machine: workspace.machine ?? null,
        path: workspace.path,
        locked: mode === "edit" && index === 0
      }))
      : [{ id: "initial-0", machine: null, path: "", locked: false }];
    return initial;
  });
  const [distros, setDistros] = useState<WslDistro[] | null>(null);
  /** Machines created from this dialog, until the caller's catalog carries them. */
  const [createdMachines, setCreatedMachines] = useState<SshMachineConfig[]>([]);
  const [machineDialogRow, setMachineDialogRow] = useState<string | null>(null);
  /** The machine whose settings its gear opened; `{ machine: null }` is this one. */
  const [machineSettings, setMachineSettings] = useState<{ machine: RunTarget | null } | null>(null);
  const [remotePickerRow, setRemotePickerRow] = useState<string | null>(null);
  const [pickingRow, setPickingRow] = useState<string | null>(null);
  const [pickErrors, setPickErrors] = useState<Record<string, string>>({});
  const [menuOpen, setMenuOpen] = useState(false);

  const machines = useMemo(() => [
    ...sshMachines,
    ...createdMachines.filter((created) => !sshMachines.some((machine) => machine.id === created.id))
  ], [createdMachines, sshMachines]);

  const loadDistros = useCallback(() => {
    // Distributions are machine state, so enumerate them each time the menu opens.
    void listWslDistros().then(setDistros).catch(() => setDistros([]));
  }, []);

  // The dialog's Escape handler runs before the menu's, so while a menu is open
  // the dialog must not treat Escape (or a backdrop click) as a request to close.
  const trackMenu = useCallback((rect: PopoverPanelRect | null) => setMenuOpen(rect !== null), []);

  const machineLabel = (machine: RunTarget | null): string => {
    if (!machine) return t("本机", "Local");
    if (machine.kind === "wsl") return machine.distro;
    return machines.find((entry) => entry.id === machine.machineId)?.name ?? t("已删除的机器", "Deleted machine");
  };

  const clearPickError = (rowId: string) => {
    setPickErrors((current) => {
      if (!(rowId in current)) return current;
      const next = { ...current };
      delete next[rowId];
      return next;
    });
  };

  const setRowMachine = (rowId: string, machine: RunTarget | null) => {
    setRows((current) => current.map((row) => (
      row.id !== rowId || row.locked || runEnvKey(row.machine) === runEnvKey(machine)
        ? row
        : { ...row, machine, path: "" }
    )));
    clearPickError(rowId);
  };

  const setRowPath = (rowId: string, path: string) => {
    setRows((current) => current.map((row) => (row.id === rowId && !row.locked ? { ...row, path } : row)));
  };

  const removeRow = (rowId: string) => {
    setRows((current) => current.filter((row, index) => index === 0 || row.locked || row.id !== rowId));
    clearPickError(rowId);
  };

  const atCap = rows.length >= MAX_PROJECT_WORKSPACES;
  const addRow = () => {
    if (atCap) return;
    setRows((current) => [...current, { id: newRowId(), machine: null, path: "", locked: false }]);
  };

  const pickLocal = async (rowId: string) => {
    setPickingRow(rowId);
    clearPickError(rowId);
    try {
      const path = await onPickLocalDirectory();
      if (path) setRowPath(rowId, path);
    } catch (error) {
      setPickErrors((current) => ({
        ...current,
        [rowId]: t("无法打开目录选择器：{error}", "Unable to open the directory picker: {error}", {
          error: errorMessage(error, t("未知错误", "Unknown error"))
        })
      }));
    } finally {
      setPickingRow((current) => (current === rowId ? null : current));
    }
  };

  const typedPath = (row: ProjectRow) => !row.machine && !nativePicker && !row.locked;
  const submittedPath = (row: ProjectRow) => (typedPath(row) ? row.path.trim() : row.path);

  /** Row id → 1-based number of the earlier row it repeats. */
  const duplicates = useMemo(() => {
    const seen = new Map<string, number>();
    const found = new Map<string, number>();
    rows.forEach((row, index) => {
      if (!row.path.trim()) return;
      const key = projectWorkspaceKey(row.machine, row.path);
      const first = seen.get(key);
      if (first === undefined) seen.set(key, index + 1);
      else found.set(row.id, first);
    });
    return found;
  }, [rows]);

  const complete = rows.every((row) => row.path.trim() !== "");
  const canSubmit = complete && duplicates.size === 0;

  const submit = () => {
    if (!canSubmit) return;
    onSubmit(name.trim(), rows.map((row) => (
      row.machine ? { machine: row.machine, path: submittedPath(row) } : { path: submittedPath(row) }
    )));
  };

  /** Deleting a machine takes it off every row still pointing at it, back to an unpicked local row. */
  const deleteMachine = (machineId: string) => {
    onDeleteSshMachine(machineId);
    setCreatedMachines((current) => current.filter((machine) => machine.id !== machineId));
    const key = runEnvKey({ kind: "ssh", machineId });
    setRows((current) => current.map((row) => (
      row.locked || runEnvKey(row.machine) !== key ? row : { ...row, machine: null, path: "" }
    )));
  };

  const settingsAction = (machine: RunTarget | null, name: string): PopoverMenuItem["action"] => ({
    label: t("{name} 的设置", "Settings for {name}", { name }),
    icon: <Settings size={13} />,
    onSelect: () => setMachineSettings({ machine })
  });

  const menuItems = (row: ProjectRow): PopoverMenuItem[] => {
    const current = runEnvKey(row.machine);
    const wslChildren: PopoverMenuItem[] = distros === null
      ? [{
        id: "wsl:loading",
        label: t("正在枚举 WSL 发行版…", "Listing WSL distributions…"),
        disabled: true
      }]
      : distros.length === 0
        ? [{ id: "wsl:none", label: t("没有 WSL 发行版", "No WSL distributions"), disabled: true }]
        : distros.map((distro): PopoverMenuItem => ({
          id: `wsl:${distro.name}`,
          label: distro.name,
          icon: <SquareTerminal size={14} />,
          hint: distro.isDefault ? t("默认", "default") : undefined,
          checked: current === `wsl:${distro.name}`,
          onSelect: () => setRowMachine(row.id, { kind: "wsl", distro: distro.name }),
          action: settingsAction({ kind: "wsl", distro: distro.name }, distro.name)
        }));
    return [
      {
        id: "local",
        label: t("本机", "Local"),
        icon: <Monitor size={14} />,
        checked: row.machine === null,
        onSelect: () => setRowMachine(row.id, null),
        action: settingsAction(null, t("本机", "Local"))
      },
      ...(showWsl
        ? [{ id: "wsl", label: "WSL", icon: <SquareTerminal size={14} />, children: wslChildren }]
        : []),
      {
        id: "ssh",
        label: "SSH",
        icon: <Server size={14} />,
        children: [
          ...machines.map((machine): PopoverMenuItem => ({
            id: `ssh:${machine.id}`,
            label: machine.name,
            icon: <Server size={14} />,
            checked: current === `ssh:${machine.id}`,
            onSelect: () => setRowMachine(row.id, { kind: "ssh", machineId: machine.id }),
            action: settingsAction({ kind: "ssh", machineId: machine.id }, machine.name)
          })),
          {
            id: "ssh:add",
            label: t("添加 SSH 机器…", "Add SSH machine…"),
            icon: <Plus size={14} />,
            onSelect: () => setMachineDialogRow(row.id)
          }
        ]
      }
    ];
  };

  const renderPath = (row: ProjectRow, index: number) => {
    if (typedPath(row)) {
      return (
        <input
          type="text"
          className="project-dialog__path-input"
          value={row.path}
          spellCheck={false}
          aria-label={t("工作区 {index} 的绝对路径", "Absolute path of workspace {index}", {
            index: String(index + 1)
          })}
          placeholder={t("绝对路径，如 C:\\Projects\\app 或 /home/me/app", "Absolute path, e.g. C:\\Projects\\app or /home/me/app")}
          onChange={(event) => setRowPath(row.id, event.target.value)}
        />
      );
    }
    const picking = pickingRow === row.id;
    return (
      <button
        type="button"
        className="composer-chip project-dialog__path"
        title={row.path || undefined}
        aria-label={row.path
          ? t("工作区 {index}：{path}", "Workspace {index}: {path}", { index: String(index + 1), path: row.path })
          : t("为工作区 {index} 选择目录", "Choose a directory for workspace {index}", { index: String(index + 1) })}
        disabled={row.locked || picking}
        onClick={() => {
          if (row.machine) setRemotePickerRow(row.id);
          else void pickLocal(row.id);
        }}
      >
        {picking ? <LoaderCircle size={13} className="spin" /> : <Folder size={13} />}
        {row.path
          ? (
            <PathText className="project-dialog__path-text" path={row.path} title={null} />
          )
          : <span className="project-dialog__path-placeholder">{t("选择目录…", "Choose a directory…")}</span>}
      </button>
    );
  };

  const remoteRow = rows.find((row) => row.id === remotePickerRow && row.machine) ?? null;

  return (
    <>
      <Dialog
        title={mode === "create" ? t("新建项目", "New project") : t("编辑项目", "Edit project")}
        description={t(
          "项目由一个或多个工作区组成，每个工作区是某台机器上的一个目录。文件、搜索和命令工具以这些目录作为安全边界。",
          "A project is one or more workspaces, each a directory on some machine. File, search, and command tools use these directories as their security boundary."
        )}
        width="560px"
        dismissible={!menuOpen}
        onClose={onClose}
        footer={<>
          <button type="button" className="button button--ghost" onClick={onClose}>
            {t("取消", "Cancel")}
          </button>
          <button type="button" className="button button--primary" disabled={!canSubmit} onClick={submit}>
            {mode === "create" && <FolderPlus size={15} />}
            {mode === "create" ? t("创建项目", "Create project") : t("保存", "Save")}
          </button>
        </>}
      >
        <label className="field">
          <span className="field__label">{t("显示名称", "Display name")}</span>
          <input
            className="input"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={t("留空时使用第一个工作区的文件夹名称", "Leave blank to use the first workspace's folder name")}
          />
        </label>
        <fieldset className="project-dialog__workspaces">
          <legend className="field__label">{t("工作区", "Workspaces")}</legend>
          <ol className="project-dialog__rows">
            {rows.map((row, index) => {
              const label = machineLabel(row.machine);
              const duplicateOf = duplicates.get(row.id);
              const error = pickErrors[row.id];
              return (
                <li className="project-dialog__row" key={row.id} data-locked={row.locked || undefined}>
                  <div className="project-dialog__row-main">
                    <PopoverMenu
                      rootClassName="project-dialog__machine"
                      triggerClassName="composer-chip"
                      trigger={<>
                        <MachineIcon machine={row.machine} size={13} />
                        <span className="composer-chip__label">{label}</span>
                        <ChevronDown size={11} className="composer-chip__caret" />
                      </>}
                      triggerLabel={t("工作区 {index} 的机器：{name}", "Machine for workspace {index}: {name}", {
                        index: String(index + 1),
                        name: label
                      })}
                      triggerTitle={row.locked
                        ? t("项目的第一个工作区不能更换", "The project's first workspace cannot be changed")
                        : undefined}
                      disabled={row.locked}
                      menuLabel={t("选择机器", "Choose a machine")}
                      menuWidth={220}
                      placement="above"
                      submenu="flyout"
                      panelClassName="project-dialog__menu"
                      onOpen={showWsl ? loadDistros : undefined}
                      onPanelRectChange={trackMenu}
                      sections={[{ id: "machines", items: menuItems(row) }]}
                    />
                    {renderPath(row, index)}
                    {index > 0 && !row.locked && (
                      <IconButton
                        label={t("移除工作区 {index}", "Remove workspace {index}", { index: String(index + 1) })}
                        className="project-dialog__remove"
                        onClick={() => removeRow(row.id)}
                      >
                        <X size={13} />
                      </IconButton>
                    )}
                  </div>
                  {duplicateOf !== undefined && (
                    <p className="project-dialog__row-error" role="alert">
                      {t(
                        "与工作区 {other} 是同一台机器上的同一个目录",
                        "Same directory on the same machine as workspace {other}",
                        { other: String(duplicateOf) }
                      )}
                    </p>
                  )}
                  {error && <p className="project-dialog__row-error" role="alert">{error}</p>}
                </li>
              );
            })}
          </ol>
          <button
            type="button"
            className="project-dialog__add"
            disabled={atCap}
            title={atCap
              ? t("一个项目最多 {max} 个工作区", "A project can have at most {max} workspaces", {
                max: String(MAX_PROJECT_WORKSPACES)
              })
              : undefined}
            onClick={addRow}
          >
            <Plus size={13} />
            {t("添加工作区", "Add workspace")}
          </button>
        </fieldset>
        <div className="safe-boundary-note">
          <Settings size={15} />
          <span>
            {t(
              "移除项目只会从 Mework 取消注册，不会删除任何本地文件。",
              "Removing a project only unregisters it from Mework; no local files are deleted."
            )}
          </span>
        </div>
      </Dialog>

      {machineDialogRow !== null && (
        <SshMachineDialog
          machine={null}
          machineCount={machines.length}
          onSave={(machine) => {
            const rowId = machineDialogRow;
            onSaveSshMachine(machine);
            setCreatedMachines((current) => [...current, machine]);
            setRowMachine(rowId, { kind: "ssh", machineId: machine.id });
            setMachineDialogRow(null);
          }}
          onClose={() => setMachineDialogRow(null)}
        />
      )}

      {machineSettings && (
        <MachineSettingsDialog
          machine={machineSettings.machine}
          sshMachines={machines}
          usage={machineUsage(machineSettings.machine)}
          shells={machineShells}
          onSaveSshMachine={(machine) => {
            onSaveSshMachine(machine);
            setCreatedMachines((current) => current.map((entry) => (entry.id === machine.id ? machine : entry)));
          }}
          onDeleteSshMachine={deleteMachine}
          onClose={() => setMachineSettings(null)}
        />
      )}

      {remoteRow?.machine && (
        <RemoteDirectoryPicker
          machine={remoteRow.machine}
          machineName={machineLabel(remoteRow.machine)}
          onPick={(path) => {
            setRowPath(remoteRow.id, path);
            setRemotePickerRow(null);
          }}
          onClose={() => setRemotePickerRow(null)}
        />
      )}
    </>
  );
}
