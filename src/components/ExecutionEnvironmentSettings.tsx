import { Monitor, Plus, Server, SquareTerminal } from "lucide-react";
import type { JSX, ReactNode } from "react";
import { useEffect, useMemo, useState } from "react";
import { useI18n } from "../i18n";
import { listWslDistros } from "../lib/runtime";
import type {
  AppDocument,
  AttachedWorkspace,
  ExecutionEnvironmentAssets,
  SshMachineConfig,
  WslDistro
} from "../types";
import { Dialog } from "./Common";
import { MAX_SSH_MACHINES, RunEnvironmentDialog, SshMachineDialog } from "./MachineDialogs";
import { SettingsPageHeading } from "./SettingsPageHeading";
import "./ExecutionEnvironmentSettings.css";

export type ExecutionEnvironmentsUpdate = (
  current: ExecutionEnvironmentAssets
) => ExecutionEnvironmentAssets;

/** One machine's variable table set, or removed when empty. Mirrors App's `saveRunEnvironmentVars`. */
export function withEnvVars(
  environments: ExecutionEnvironmentAssets,
  envKey: string,
  vars: Record<string, string>
): ExecutionEnvironmentAssets {
  const envVars = { ...environments.envVars };
  if (Object.keys(vars).length) envVars[envKey] = vars;
  else delete envVars[envKey];
  return { ...environments, envVars };
}

/** Adds or replaces one SSH machine together with its variable table. */
export function withSshMachine(
  environments: ExecutionEnvironmentAssets,
  machine: SshMachineConfig,
  vars: Record<string, string>
): ExecutionEnvironmentAssets {
  const exists = environments.sshMachines.some((item) => item.id === machine.id);
  return withEnvVars({
    ...environments,
    sshMachines: exists
      ? environments.sshMachines.map((item) => (item.id === machine.id ? machine : item))
      : [...environments.sshMachines, machine]
  }, `ssh:${machine.id}`, vars);
}

/** Removes a machine and its variable table. Mirrors App's `deleteSshMachine`. */
export function withoutSshMachine(
  environments: ExecutionEnvironmentAssets,
  machineId: string
): ExecutionEnvironmentAssets {
  const envVars = { ...environments.envVars };
  delete envVars[`ssh:${machineId}`];
  return {
    sshMachines: environments.sshMachines.filter((item) => item.id !== machineId),
    envVars
  };
}

/**
 * How many projects and conversations still name an SSH machine: a project by
 * its first workspace or any of its additional ones, a conversation by one of
 * its attached workspaces.
 */
export function sshMachineUsage(
  document: AppDocument | null | undefined,
  machineId: string
): { projects: number; conversations: number } {
  const onMachine = (entry: Pick<AttachedWorkspace, "machine">) => (
    entry.machine?.kind === "ssh" && entry.machine.machineId === machineId
  );
  let projects = 0;
  let conversations = 0;
  for (const project of document?.workspaces ?? []) {
    if (onMachine(project) || (project.additionalWorkspaces ?? []).some(onMachine)) projects += 1;
    for (const conversation of project.conversations) {
      if ((conversation.attachedWorkspaces ?? []).some(onMachine)) conversations += 1;
    }
  }
  return { projects, conversations };
}

function sshAddress(machine: SshMachineConfig): string {
  return machine.port ? `${machine.host}:${machine.port}` : machine.host;
}

interface EnvDialogState {
  envKey: string;
  name: string;
}

/**
 * The machines shell commands can run on — this one, each WSL distribution,
 * each registered SSH machine — with the environment variables injected there.
 *
 * Which machine a command runs on is not chosen here: it follows from the
 * workspace the command runs in. This page only describes the machines.
 */
export function ExecutionEnvironmentSettings({
  environments,
  document = null,
  onChange
}: {
  environments: ExecutionEnvironmentAssets;
  /** Read to say what still uses a machine before it is deleted. */
  document?: AppDocument | null;
  onChange: (update: ExecutionEnvironmentsUpdate) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [distros, setDistros] = useState<WslDistro[] | null>(null);
  const [envDialog, setEnvDialog] = useState<EnvDialogState | null>(null);
  /** `{ machine: null }` creates a machine; `null` is no dialog. */
  const [machineDialog, setMachineDialog] = useState<{ machine: SshMachineConfig | null } | null>(null);
  const [pendingDelete, setPendingDelete] = useState<SshMachineConfig | null>(null);

  useEffect(() => {
    let cancelled = false;
    // WSL distributions are machine state, so they are enumerated live rather than stored.
    void listWslDistros()
      .then((next) => {
        if (!cancelled) setDistros(next);
      })
      .catch(() => {
        if (!cancelled) setDistros([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const envCount = (envKey: string): number => Object.keys(environments.envVars[envKey] ?? {}).length;
  const envSummary = (envKey: string): string => {
    const count = envCount(envKey);
    return count
      ? t("{count} 个环境变量", "{count} environment variables", { count: String(count) })
      : t("没有环境变量", "No environment variables");
  };

  const usageNote = (machineId: string): string | undefined => {
    const usage = sshMachineUsage(document, machineId);
    if (!usage.projects && !usage.conversations) return undefined;
    return t(
      "{projects} 个项目、{conversations} 个对话仍在使用这台机器",
      "{projects} projects and {conversations} conversations still use this machine",
      { projects: String(usage.projects), conversations: String(usage.conversations) }
    );
  };

  const pendingUsage = useMemo(
    () => (pendingDelete ? sshMachineUsage(document, pendingDelete.id) : null),
    [document, pendingDelete]
  );

  const row = (options: {
    key: string;
    icon: ReactNode;
    name: string;
    detail?: string;
    /** The detail is an address, set in the code face. */
    detailIsAddress?: boolean;
    envKey: string;
    action: ReactNode;
  }) => (
    <li className="execution-env__row" key={options.key}>
      <span className="execution-env__icon" aria-hidden="true">{options.icon}</span>
      <span className="execution-env__copy">
        <strong>{options.name}</strong>
        {options.detail && (
          <small className={options.detailIsAddress ? "execution-env__address" : undefined}>{options.detail}</small>
        )}
      </span>
      <span
        className={`execution-env__count${envCount(options.envKey) ? "" : " execution-env__count--empty"}`}
      >
        {envSummary(options.envKey)}
      </span>
      {options.action}
    </li>
  );

  const editVariablesButton = (envKey: string, name: string) => (
    <button
      type="button"
      className="button button--secondary button--small execution-env__action"
      aria-label={t("编辑 {name} 的环境变量", "Edit environment variables for {name}", { name })}
      onClick={() => setEnvDialog({ envKey, name })}
    >
      {t("编辑变量", "Edit variables")}
    </button>
  );

  const machinesFull = environments.sshMachines.length >= MAX_SSH_MACHINES;

  return (
    <section className="settings-page execution-env">
      <SettingsPageHeading
        title={t("执行环境", "Execution environments")}
        description={t(
          "命令在哪台机器上执行，由它所在的工作区决定。这里管理那些机器：各自注入的环境变量，以及登记的 SSH 机器。",
          "Which machine a command runs on follows from the workspace it runs in. This page manages those machines: the environment variables injected on each, and the registered SSH machines."
        )}
      />

      <div className="execution-env__section">
        <h4 className="execution-env__section-title">{t("本机", "This machine")}</h4>
        <ul className="execution-env__list">
          {row({
            key: "local",
            icon: <Monitor size={16} />,
            name: t("本机", "This machine"),
            detail: t("运行 Mework 的这台电脑", "The computer running Mework"),
            envKey: "local",
            action: editVariablesButton("local", t("本机", "This machine"))
          })}
        </ul>
      </div>

      {Boolean(distros?.length) && (
        <div className="execution-env__section">
          <h4 className="execution-env__section-title">{t("WSL 发行版", "WSL distributions")}</h4>
          <ul className="execution-env__list">
            {distros!.map((distro) => row({
              key: `wsl:${distro.name}`,
              icon: <SquareTerminal size={16} />,
              name: distro.name,
              detail: distro.isDefault
                ? t("WSL {version} · 默认", "WSL {version} · default", { version: String(distro.version) })
                : t("WSL {version}", "WSL {version}", { version: String(distro.version) }),
              envKey: `wsl:${distro.name}`,
              action: editVariablesButton(`wsl:${distro.name}`, distro.name)
            }))}
          </ul>
        </div>
      )}

      <div className="execution-env__section">
        <div className="execution-env__section-heading">
          <h4 className="execution-env__section-title">{t("SSH 机器", "SSH machines")}</h4>
          <button
            type="button"
            className="button button--secondary button--small execution-env__add"
            disabled={machinesFull}
            title={machinesFull
              ? t("最多只能登记 {max} 台 SSH 机器", "At most {max} SSH machines can be registered", {
                max: String(MAX_SSH_MACHINES)
              })
              : undefined}
            onClick={() => setMachineDialog({ machine: null })}
          >
            <Plus size={13} />
            {t("添加 SSH 机器…", "Add SSH machine…")}
          </button>
        </div>
        {environments.sshMachines.length > 0
          ? (
            <ul className="execution-env__list">
              {environments.sshMachines.map((machine) => row({
                key: `ssh:${machine.id}`,
                icon: <Server size={16} />,
                name: machine.name,
                detail: sshAddress(machine),
                detailIsAddress: true,
                envKey: `ssh:${machine.id}`,
                action: (
                  <button
                    type="button"
                    className="button button--secondary button--small execution-env__action"
                    aria-label={t("编辑 SSH 机器 {name}", "Edit SSH machine {name}", { name: machine.name })}
                    onClick={() => setMachineDialog({ machine })}
                  >
                    {t("编辑", "Edit")}
                  </button>
                )
              }))}
            </ul>
          )
          : (
            <p className="execution-env__empty">
              {t(
                "还没有登记 SSH 机器。登记后，项目就能在那台机器上选目录作为工作区。",
                "No SSH machines yet. Once one is registered, a project can use a directory on it as a workspace."
              )}
            </p>
          )}
      </div>

      {envDialog && (
        <RunEnvironmentDialog
          title={t("{name} 的环境变量", "Environment variables for {name}", { name: envDialog.name })}
          vars={environments.envVars[envDialog.envKey] ?? {}}
          onSave={(vars) => {
            const { envKey } = envDialog;
            onChange((current) => withEnvVars(current, envKey, vars));
            setEnvDialog(null);
          }}
          onClose={() => setEnvDialog(null)}
        />
      )}

      {machineDialog && (
        <SshMachineDialog
          machine={machineDialog.machine}
          vars={machineDialog.machine ? environments.envVars[`ssh:${machineDialog.machine.id}`] ?? {} : {}}
          inUseNote={machineDialog.machine ? usageNote(machineDialog.machine.id) : undefined}
          machineCount={environments.sshMachines.length}
          onSave={(machine, vars) => {
            onChange((current) => withSshMachine(current, machine, vars));
            setMachineDialog(null);
          }}
          onDelete={() => setPendingDelete(machineDialog.machine)}
          onClose={() => setMachineDialog(null)}
        />
      )}

      {pendingDelete && pendingUsage && (
        <Dialog
          title={t("删除 SSH 机器“{name}”？", "Delete SSH machine “{name}”?", { name: pendingDelete.name })}
          width="420px"
          onClose={() => setPendingDelete(null)}
          footer={<>
            <button type="button" className="button button--secondary" onClick={() => setPendingDelete(null)}>
              {t("取消", "Cancel")}
            </button>
            <button
              type="button"
              className="button button--danger"
              onClick={() => {
                const machineId = pendingDelete.id;
                onChange((current) => withoutSshMachine(current, machineId));
                setPendingDelete(null);
                setMachineDialog(null);
              }}
            >
              {t("删除", "Delete")}
            </button>
          </>}
        >
          <p className="confirm-copy">
            {pendingUsage.projects || pendingUsage.conversations
              ? t(
                "{projects} 个项目、{conversations} 个对话仍在使用这台机器。删除后，它们在这台机器上的工作区都会失效，直到重新选择。",
                "{projects} projects and {conversations} conversations still use this machine. Their workspaces on it stop working until they are chosen again.",
                {
                  projects: String(pendingUsage.projects),
                  conversations: String(pendingUsage.conversations)
                }
              )
              : t("没有项目或对话在使用这台机器。", "No project or conversation uses this machine.")}
          </p>
          <p className="confirm-copy execution-env__confirm-note">
            {t("它的环境变量会一并删除。", "Its environment variables are deleted with it.")}
          </p>
        </Dialog>
      )}
    </section>
  );
}
