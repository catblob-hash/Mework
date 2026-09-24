import { useEffect, useState } from "react";
import { Dialog } from "./Common";
import { useI18n } from "../i18n";
import { createId } from "../lib/id";
import { listWslDistros } from "../lib/runtime";
import {
  effectiveAgentShell,
  machineOsLabel,
  shellBackendLabel
} from "../lib/machineShells";
import { runEnvKey, type MachineUsage } from "../lib/workspaces";
import type {
  ExecutionEnvironmentAssets,
  MachineShells,
  RunTarget,
  ShellBackend,
  SshMachineConfig,
  WslDistro
} from "../types";

/**
 * What the machine dialogs need to show and change a machine's shells: every
 * machine's last probe, the settings its agent shell is read from, a way to
 * probe a machine now, and a way to record a WSL distribution's agent shell
 * (an SSH machine saves its own with the rest of its dialog).
 */
export interface MachineShellsControl {
  probes: Readonly<Record<string, MachineShells>>;
  environments: ExecutionEnvironmentAssets;
  /** Probes `machine` (`null` is this one) and resolves with the answer, or rejects with why not. */
  probe: (machine: RunTarget | null) => Promise<MachineShells>;
  setAgentShell: (machine: RunTarget, backend: ShellBackend) => void;
}

interface MachineShellsSectionProps {
  machine: RunTarget | null;
  shells: MachineShellsControl;
  /**
   * The agent shell to show as chosen and where a choice goes, for a machine
   * with an agent — a WSL distribution or an SSH machine. Absent for this
   * machine, whose file tools act on its own filesystem and run no scripts.
   */
  agentShell?: {
    value: ShellBackend | null;
    onChange: (backend: ShellBackend) => void;
  };
}

/**
 * A machine's shell backends: a button that probes the machine again, the
 * agent-shell choice to its right, and what the last probe found.
 */
function MachineShellsSection({ machine, shells, agentShell }: MachineShellsSectionProps) {
  const { t } = useI18n();
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const probed = shells.probes[runEnvKey(machine)];
  const found = probed?.shells ?? [];
  const probe = () => {
    setProbing(true);
    setError(null);
    void shells.probe(machine)
      .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setProbing(false));
  };
  const chosen = agentShell?.value ?? null;
  return (
    <section className="machine-shells" aria-label={t("Shell 后端", "Shell backends")}>
      <div className="machine-shells__row">
        <button
          type="button"
          className="button button--secondary"
          disabled={probing}
          onClick={probe}
        >
          {probing ? t("正在探测…", "Probing…") : t("重新探测 shell", "Probe shells again")}
        </button>
        {agentShell && (
          <label className="machine-shells__agent">
            <span>{t("代理 shell", "Agent shell")}</span>
            <select
              className="input"
              value={chosen ?? ""}
              disabled={probing || found.length === 0}
              title={t(
                "这台机器上的代理用它运行 Mework 自己的脚本：远程文件工具与语言服务器。",
                "The shell this machine's agent runs Mework's own scripts in: the remote file tools and language servers."
              )}
              onChange={(event) => agentShell.onChange(event.target.value as ShellBackend)}
            >
              {found.length === 0 && (
                <option value={chosen ?? ""}>
                  {chosen ? shellBackendLabel(chosen) : t("先探测", "Probe first")}
                </option>
              )}
              {found.map((shell) => (
                <option key={shell.backend} value={shell.backend}>{shellBackendLabel(shell.backend)}</option>
              ))}
            </select>
          </label>
        )}
      </div>
      {error
        ? <p className="run-location__error" role="alert">{error}</p>
        : probed
          ? (
            <dl className="machine-settings__facts machine-shells__found">
              <dt>{t("系统", "System")}</dt>
              <dd>{machineOsLabel(probed.os)}</dd>
              <dt>{t("可用 shell", "Shells")}</dt>
              <dd>
                {found.length === 0
                  ? t("没有 Mework 能在这个系统上使用的 shell", "No shell Mework can use on this system")
                  : found.map((shell) => (
                    <span key={shell.backend} className="machine-shells__shell" title={shell.path}>
                      {shellBackendLabel(shell.backend)}
                      <code>{shell.path}</code>
                    </span>
                  ))}
              </dd>
            </dl>
          )
          : (
            <p className="machine-shells__pending">
              {!machine
                ? t(
                  "还没有探测结果。Mework 启动时会探测本机，也可以现在探测。",
                  "No probe yet. Mework probes this machine at startup, or probe it now."
                )
                : machine.kind === "wsl"
                  ? t(
                    "还没有探测过这个发行版。首次在它上面运行时会自动探测，也可以现在探测。",
                    "This distribution has not been probed yet. It is probed the first time something runs on it, or probe it now."
                  )
                  : t(
                    "还没有探测过这台机器。首次连接时会自动探测，也可以现在探测。",
                    "This machine has not been probed yet. It is probed on first connection, or probe it now."
                  )}
            </p>
          )}
    </section>
  );
}

/** Mirrors the host `validate_env_var_name` predicate for field-level validation. */
const ENV_NAME_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Mirrors host-reserved shell-startup and private child-environment names.
 * The host rejects an entire document containing these names, so reject them at
 * the field rather than on save. */
function reservedEnvVarName(name: string): boolean {
  if ([
    "BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS", "CDPATH", "GLOBIGNORE", "GIT_EXTERNAL_DIFF"
  ].includes(name)) return true;
  const upper = name.toUpperCase();
  return (upper.startsWith("MEWORK_") || upper.startsWith("VITE_"))
    && (upper.includes("BROWSER_DEV") || upper.includes("E2E"));
}

/** Mirrors limits enforced by host `validate_execution_environments`. */
const MAX_ENV_VARS_PER_TABLE = 128;
const MAX_ENV_VALUE_CHARS = 8192;
const MAX_SSH_MACHINES = 64;
const MAX_MACHINE_NAME_CHARS = 64;
const MAX_HOST_CHARS = 512;
const MAX_PATH_FIELD_CHARS = 4096;

const CONTROL_CHARS = /[\u0000-\u001f\u007f]/;

function formatEnvText(vars: Record<string, string>): string {
  return Object.entries(vars).map(([key, value]) => `${key}=${value}`).join("\n");
}

interface ParsedEnvText {
  vars: Record<string, string>;
  /** Lines with invalid variable-name syntax. */
  invalid: string[];
  /** Lines with host-reserved variable names. */
  reserved: string[];
  /** Lines whose values are too long or contain control characters. */
  badValues: string[];
  tooMany: boolean;
}

/** Parses `KEY=value` lines. Preserve everything after the first `=` verbatim;
 * only keys are trimmed. */
function parseEnvText(text: string): ParsedEnvText {
  const vars: Record<string, string> = {};
  const invalid: string[] = [];
  const reserved: string[] = [];
  const badValues: string[] = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const position = line.indexOf("=");
    const key = position < 0 ? line.trim() : line.slice(0, position).trim();
    if (!key) continue;
    if (position < 0 || !ENV_NAME_PATTERN.test(key) || key.length > 128) {
      invalid.push(key || line.trim());
      continue;
    }
    if (reservedEnvVarName(key)) {
      reserved.push(key);
      continue;
    }
    const value = line.slice(position + 1);
    if (value.length > MAX_ENV_VALUE_CHARS || CONTROL_CHARS.test(value)) {
      badValues.push(key);
      continue;
    }
    vars[key] = value;
  }
  return {
    vars,
    invalid,
    reserved,
    badValues,
    tooMany: Object.keys(vars).length > MAX_ENV_VARS_PER_TABLE
  };
}

/**
 * Field-level validation for the environment-variable editor. The host rejects
 * an entire document with invalid execution environments, so errors stay local.
 */
function envTextError(
  parsed: ParsedEnvText,
  t: (zh: string, en: string, params?: Record<string, string>) => string
): string | null {
  if (parsed.invalid.length) {
    return t(
      "变量名不合法：{names}（需以字母或下划线开头，只含字母、数字、下划线，最长 128）",
      "Invalid variable names: {names} (must start with a letter or underscore, contain only letters, digits and underscores, max 128)",
      { names: parsed.invalid.join(", ") }
    );
  }
  if (parsed.reserved.length) {
    return t(
      "这些变量名由宿主保留，不能配置：{names}",
      "These variable names are reserved by the host and cannot be configured: {names}",
      { names: parsed.reserved.join(", ") }
    );
  }
  if (parsed.badValues.length) {
    return t(
      "这些变量的值过长或含控制字符：{names}",
      "The values of these variables are too long or contain control characters: {names}",
      { names: parsed.badValues.join(", ") }
    );
  }
  if (parsed.tooMany) {
    return t(
      "一个工作区最多 {max} 条变量",
      "A workspace can hold at most {max} variables",
      { max: String(MAX_ENV_VARS_PER_TABLE) }
    );
  }
  return null;
}

export interface WorkspaceEnvironmentDialogProps {
  title: string;
  vars: Record<string, string>;
  onSave: (vars: Record<string, string>) => void;
  onClose: () => void;
}

/** A workspace's environment-variable editor. The textarea holds its raw draft so
 * incremental edits are not normalized away. */
export function WorkspaceEnvironmentDialog({
  title,
  vars,
  onSave,
  onClose
}: WorkspaceEnvironmentDialogProps) {
  const { t } = useI18n();
  const [text, setText] = useState(() => formatEnvText(vars));
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog
      title={title}
      description={t(
        "每行一条 KEY=value。这些变量会注入在这个工作区里执行的 shell 命令，同一台机器上的其他工作区不受影响。",
        "One KEY=value per line. These variables are injected into shell commands run in this workspace; other workspaces on the same machine are unaffected."
      )}
      width="420px"
      onClose={onClose}
      footer={<>
        <button type="button" className="button button--secondary" onClick={onClose}>
          {t("取消", "Cancel")}
        </button>
        <button
          type="button"
          className="button button--primary"
          onClick={() => {
            const parsed = parseEnvText(text);
            const message = envTextError(parsed, t);
            if (message) {
              setError(message);
              return;
            }
            onSave(parsed.vars);
          }}
        >
          {t("保存", "Save")}
        </button>
      </>}
    >
      <textarea
        className="run-location__env-input"
        rows={8}
        value={text}
        aria-label={t("环境变量", "Environment variables")}
        placeholder={"API_KEY=value\nHTTP_PROXY=http://127.0.0.1:7890"}
        spellCheck={false}
        onChange={(event) => {
          setText(event.target.value);
          setError(null);
        }}
      />
      {error && <p className="run-location__error" role="alert">{error}</p>}
    </Dialog>
  );
}

export interface SshMachineDialogProps {
  /** Null when creating a machine. */
  machine: SshMachineConfig | null;
  /**
   * What still uses this machine, when something does. Shown as the delete
   * button's tooltip so the consequence is visible before the click.
   */
  inUseNote?: string;
  /** Registered machine count, used to enforce the host limit during creation. */
  machineCount: number;
  /** A saved machine's shells; absent while the machine is being created. */
  shells?: MachineShellsControl;
  onSave: (machine: SshMachineConfig) => void;
  /** Omitted where a machine cannot be deleted from; the delete button then never shows. */
  onDelete?: (machineId: string) => void;
  onClose: () => void;
}

/**
 * SSH machine creation and editing dialog. Environment variables are not set
 * here: they belong to each workspace on the machine, not to the machine.
 */
export function SshMachineDialog({
  machine,
  inUseNote,
  machineCount,
  shells,
  onSave,
  onDelete,
  onClose
}: SshMachineDialogProps) {
  const { t } = useI18n();
  const target: RunTarget | null = machine ? { kind: "ssh", machineId: machine.id } : null;
  const [agentShell, setAgentShell] = useState<ShellBackend | null>(() => (
    target && shells ? effectiveAgentShell(target, shells.environments, shells.probes) : null
  ));
  const [name, setName] = useState(machine?.name ?? "");
  const [host, setHost] = useState(machine?.host ?? "");
  const [port, setPort] = useState(machine && machine.port !== 0 ? String(machine.port) : "");
  const [identityFile, setIdentityFile] = useState(machine?.identityFile ?? "");
  const [error, setError] = useState<string | null>(null);

  const field = (
    label: string,
    value: string,
    onChange: (value: string) => void,
    placeholder?: string
  ) => (
    <label className="run-location__field">
      <span>{label}</span>
      <input
        type="text"
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(event) => {
          onChange(event.target.value);
          setError(null);
        }}
      />
    </label>
  );

  return (
    <Dialog
      title={machine
        ? t("配置 SSH 机器", "Configure SSH machine")
        : t("添加 SSH 机器", "Add SSH machine")}
      description={t(
        "认证材料不落盘：连接时由 OpenSSH 按身份文件、~/.ssh/config 与 agent 解析。工作目录和环境变量不在这里设——它们属于工作区：在项目里为这台机器选一个目录。",
        "No credentials are stored: OpenSSH resolves the identity file, ~/.ssh/config and the agent at connect time. The working directory and environment variables are not set here — they belong to a workspace: pick a directory on this machine in a project."
      )}
      width="460px"
      onClose={onClose}
      footer={<>
        {machine && onDelete && (
          <button
            type="button"
            className="button button--secondary run-location__delete"
            title={inUseNote}
            onClick={() => onDelete(machine.id)}
          >
            {t("删除", "Delete")}
          </button>
        )}
        <button type="button" className="button button--secondary" onClick={onClose}>
          {t("取消", "Cancel")}
        </button>
        <button
          type="button"
          className="button button--primary"
          onClick={() => {
            const trimmedName = name.trim();
            const trimmedHost = host.trim();
            if (!trimmedName || !trimmedHost) {
              setError(t("名称与主机不能为空", "Name and host must not be empty"));
              return;
            }
            // Mirror `validate_execution_environments`: configurations the host
            // rejects must not make the entire document unsaveable.
            if (trimmedName.length > MAX_MACHINE_NAME_CHARS) {
              setError(t(
                "名称最长 {max} 个字符", "The name can be at most {max} characters",
                { max: String(MAX_MACHINE_NAME_CHARS) }
              ));
              return;
            }
            if (/\s/.test(trimmedHost) || trimmedHost.startsWith("-")) {
              setError(t("主机地址不能包含空白或以 - 开头", "The host must not contain whitespace or start with -"));
              return;
            }
            if (trimmedHost.length > MAX_HOST_CHARS || CONTROL_CHARS.test(trimmedHost)) {
              setError(t(
                "主机地址过长或含控制字符（最长 {max}）",
                "The host is too long or contains control characters (max {max})",
                { max: String(MAX_HOST_CHARS) }
              ));
              return;
            }
            const trimmedIdentity = identityFile.trim();
            if (trimmedIdentity.length > MAX_PATH_FIELD_CHARS || CONTROL_CHARS.test(trimmedIdentity)) {
              setError(t(
                "身份文件路径过长或含控制字符（最长 {max}）",
                "The identity file path is too long or contains control characters (max {max})",
                { max: String(MAX_PATH_FIELD_CHARS) }
              ));
              return;
            }
            if (!machine && machineCount >= MAX_SSH_MACHINES) {
              setError(t(
                "最多只能登记 {max} 台 SSH 机器",
                "At most {max} SSH machines can be registered",
                { max: String(MAX_SSH_MACHINES) }
              ));
              return;
            }
            const parsedPort = port.trim() === "" ? 0 : Number(port.trim());
            if (!Number.isInteger(parsedPort) || parsedPort < 0 || parsedPort > 65535) {
              setError(t("端口必须是 0–65535 的整数", "The port must be an integer between 0 and 65535"));
              return;
            }
            const now = new Date().toISOString();
            const keptAgentShell = agentShell ?? machine?.agentShell;
            onSave({
              id: machine?.id ?? createId("sshm"),
              name: trimmedName,
              host: trimmedHost,
              port: parsedPort,
              identityFile: trimmedIdentity,
              ...(keptAgentShell ? { agentShell: keptAgentShell } : {}),
              createdAt: machine?.createdAt ?? now,
              updatedAt: now
            });
          }}
        >
          {t("保存", "Save")}
        </button>
      </>}
    >
      <div className="run-location__form">
        {field(t("名称", "Name"), name, setName, t("开发机", "devbox"))}
        {field(t("主机", "Host"), host, setHost, "user@hostname")}
        {field(t("端口（空 = 22）", "Port (empty = 22)"), port, setPort, "22")}
        {field(t("身份文件（可选）", "Identity file (optional)"), identityFile, setIdentityFile, "~/.ssh/id_ed25519")}
      </div>
      {target && shells && (
        <MachineShellsSection
          machine={target}
          shells={shells}
          agentShell={{
            // A probe that arrives while the dialog is open fills in the
            // choice it would record, unless one was already made here.
            value: agentShell ?? effectiveAgentShell(target, shells.environments, shells.probes),
            onChange: setAgentShell
          }}
        />
      )}
      {error && <p className="run-location__error" role="alert">{error}</p>}
    </Dialog>
  );
}

export interface MachineSettingsDialogProps {
  /** The machine to show: `null` is this one. */
  machine: RunTarget | null;
  sshMachines: readonly SshMachineConfig[];
  /** What still has a workspace on the machine, stated before it is deleted. */
  usage: MachineUsage;
  shells: MachineShellsControl;
  onSaveSshMachine: (machine: SshMachineConfig) => void;
  onDeleteSshMachine: (machineId: string) => void;
  onClose: () => void;
}

/**
 * A machine's settings, opened from the gear beside it wherever machines are
 * listed. An SSH machine is edited or deleted here. This machine and a WSL
 * distribution are found, not registered, so theirs says what they are and
 * what is on them. Every machine shows its shell backends and can be probed
 * again; a WSL distribution and an SSH machine also choose their agent shell.
 *
 * Environment variables are never here: they belong to each workspace.
 */
export function MachineSettingsDialog({
  machine,
  sshMachines,
  usage,
  shells,
  onSaveSshMachine,
  onDeleteSshMachine,
  onClose
}: MachineSettingsDialogProps) {
  const { t } = useI18n();
  const [pendingDelete, setPendingDelete] = useState<SshMachineConfig | null>(null);
  const [distro, setDistro] = useState<WslDistro | null>(null);
  const distroName = machine?.kind === "wsl" ? machine.distro : null;

  useEffect(() => {
    if (!distroName) return;
    let cancelled = false;
    // Distributions are machine state, so they are enumerated live rather than stored.
    void listWslDistros()
      .then((distros) => {
        if (!cancelled) setDistro(distros.find((entry) => entry.name === distroName) ?? null);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [distroName]);

  const inUse = usage.projects > 0 || usage.conversations > 0;
  const usageText = inUse
    ? t(
      "{projects} 个项目、{conversations} 个对话在这台机器上有工作区",
      "{projects} projects and {conversations} conversations have workspaces on this machine",
      { projects: String(usage.projects), conversations: String(usage.conversations) }
    )
    : t("还没有工作区在这台机器上", "No workspace is on this machine yet");

  const sshMachine = machine?.kind === "ssh"
    ? sshMachines.find((entry) => entry.id === machine.machineId) ?? null
    : null;

  if (sshMachine) {
    if (pendingDelete) {
      return (
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
                onDeleteSshMachine(pendingDelete.id);
                onClose();
              }}
            >
              {t("删除", "Delete")}
            </button>
          </>}
        >
          <p className="confirm-copy">
            {inUse
              ? t(
                "{projects} 个项目、{conversations} 个对话仍在使用这台机器。删除后，它们在这台机器上的工作区都会失效，直到重新选择。",
                "{projects} projects and {conversations} conversations still use this machine. Their workspaces on it stop working until they are chosen again.",
                { projects: String(usage.projects), conversations: String(usage.conversations) }
              )
              : t("没有项目或对话在使用这台机器。", "No project or conversation uses this machine.")}
          </p>
        </Dialog>
      );
    }
    return (
      <SshMachineDialog
        machine={sshMachine}
        inUseNote={inUse ? usageText : undefined}
        machineCount={sshMachines.length}
        shells={shells}
        onSave={(next) => {
          onSaveSshMachine(next);
          onClose();
        }}
        onDelete={() => setPendingDelete(sshMachine)}
        onClose={onClose}
      />
    );
  }

  const title = !machine
    ? t("本机", "This machine")
    : machine.kind === "wsl"
      ? machine.distro
      : t("已删除的机器", "Deleted machine");
  const description = !machine
    ? t(
      "运行 Mework 的这台电脑。它随 Mework 而在，不需要登记，也没有要配置的连接。",
      "The computer running Mework. It is always there, needs no registration, and has no connection to configure."
    )
    : machine.kind === "wsl"
      ? t(
        "这台电脑上的一个 WSL 发行版。发行版由系统枚举，不需要登记，也没有要配置的连接。",
        "A WSL distribution on this computer. Distributions are enumerated from the system, need no registration, and have no connection to configure."
      )
      : t(
        "这台 SSH 机器已从登记中删除，它上面的工作区在重新选择之前都无法使用。",
        "This SSH machine was deleted from the catalog; its workspaces cannot be used until they are chosen again."
      );
  const kind = !machine
    ? t("本机", "This machine")
    : machine.kind === "wsl"
      ? distro
        ? distro.isDefault
          ? t("WSL {version} · 默认", "WSL {version} · default", { version: String(distro.version) })
          : t("WSL {version}", "WSL {version}", { version: String(distro.version) })
        : "WSL"
      : "SSH";

  return (
    <Dialog
      title={title}
      description={description}
      width="420px"
      onClose={onClose}
      footer={
        <button type="button" className="button button--secondary" onClick={onClose}>
          {t("关闭", "Close")}
        </button>
      }
    >
      <dl className="machine-settings__facts">
        <dt>{t("类型", "Kind")}</dt>
        <dd>{kind}</dd>
        <dt>{t("工作区", "Workspaces")}</dt>
        <dd>{usageText}</dd>
      </dl>
      {(!machine || machine.kind === "wsl") && (
        <MachineShellsSection
          machine={machine}
          shells={shells}
          agentShell={machine?.kind === "wsl"
            ? {
              value: effectiveAgentShell(machine, shells.environments, shells.probes),
              onChange: (backend) => shells.setAgentShell(machine, backend)
            }
            : undefined}
        />
      )}
      <p className="machine-settings__note">
        {t(
          "环境变量属于工作区，不属于机器：在输入框上方的工作区菜单里，点工作区右侧的齿轮来设置。",
          "Environment variables belong to a workspace, not to its machine: set them from the gear beside a workspace in the workspace menu above the message box."
        )}
      </p>
    </Dialog>
  );
}
