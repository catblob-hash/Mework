import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown, Monitor, Plus, Server, Settings2, SquareTerminal } from "lucide-react";
import { Dialog, IconButton } from "./Common";
import { usePopoverAnchor } from "./usePopoverAnchor";
import { useI18n } from "../i18n";
import { listWslDistros } from "../lib/runtime";
import { createId } from "../lib/id";
import type { RunTarget, SshMachineConfig, WslDistro } from "../types";

const PANEL_WIDTH = 268;

/** Address key for an environment-variable table. Matches host `run_environment::env_key` exactly. */
export function runEnvKey(target: RunTarget | null): string {
  if (!target) return "local";
  return target.kind === "wsl" ? `wsl:${target.distro}` : `ssh:${target.machineId}`;
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
 * Shared field-level validation for both environment dialogs. The host rejects
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
      "一个环境最多 {max} 条变量",
      "An environment can hold at most {max} variables",
      { max: String(MAX_ENV_VARS_PER_TABLE) }
    );
  }
  return null;
}

interface EnvDialogState {
  /** Environment key: `local`, `wsl:<distro>`, or `ssh:<machine id>`. */
  envKey: string;
  /** Environment name used in the dialog title. */
  name: string;
}

interface MachineDialogState {
  /** Null when creating a machine. */
  machine: SshMachineConfig | null;
}

export interface RunLocationPickerProps {
  runTarget: RunTarget | null;
  sshMachines: SshMachineConfig[];
  envVars: Record<string, Record<string, string>>;
  /** The run location cannot change during a conversation; environment edits apply to later turns. */
  selectionDisabled: boolean;
  onSelect: (target: RunTarget | null) => void;
  onSaveEnvVars: (envKey: string, vars: Record<string, string>) => void;
  onSaveMachine: (machine: SshMachineConfig) => void;
  onDeleteMachine: (machineId: string) => void;
}

/**
 * Selects a local, WSL, or SSH run location. Environment variables are edited
 * per location; SSH rows also edit their machine configuration.
 *
 * This cannot use `PopoverMenu`: its rows are buttons and cannot contain an
 * independent gear button. Render the panel through a portal because a
 * transformed sidebar ancestor would otherwise clip it.
 */
export function RunLocationPicker({
  runTarget,
  sshMachines,
  envVars,
  selectionDisabled,
  onSelect,
  onSaveEnvVars,
  onSaveMachine,
  onDeleteMachine
}: RunLocationPickerProps) {
  const { t } = useI18n();
  const [distros, setDistros] = useState<WslDistro[] | null>(null);
  const { open, position, triggerRef, panelRef, toggle, close } = usePopoverAnchor({
    width: PANEL_WIDTH,
    onOpen: () => {
      // WSL distributions are machine state, so enumerate them on each open.
      void listWslDistros().then(setDistros).catch(() => setDistros([]));
    }
  });
  const [envDialog, setEnvDialog] = useState<EnvDialogState | null>(null);
  const [machineDialog, setMachineDialog] = useState<MachineDialogState | null>(null);

  const positioned = position !== null;
  useEffect(() => {
    if (!open || !positioned) return;
    panelRef.current?.focus({ preventScroll: true });
  }, [open, positioned, panelRef]);

  const activeKey = runEnvKey(runTarget);
  const activeMachine = runTarget?.kind === "ssh"
    ? sshMachines.find((machine) => machine.id === runTarget.machineId) ?? null
    : null;
  const triggerText = !runTarget
    ? t("本机", "Local")
    : runTarget.kind === "wsl"
      ? runTarget.distro
      : activeMachine?.name ?? t("已删除的机器", "Deleted machine");
  const TriggerIcon = !runTarget ? Monitor : runTarget.kind === "wsl" ? SquareTerminal : Server;

  const envCount = (key: string): number => Object.keys(envVars[key] ?? {}).length;

  const row = (options: {
    key: string;
    icon: ReactNode;
    label: string;
    hint?: string;
    checked: boolean;
    target: RunTarget | null;
    envKey: string;
    envName: string;
    machine?: SshMachineConfig;
  }) => (
    <div className="run-location__row" key={options.key} data-checked={options.checked || undefined}>
      <button
        type="button"
        className="run-location__select"
        disabled={selectionDisabled}
        title={selectionDisabled
          ? t("对话正在运行，暂时不能更换运行地点", "The conversation is running, so the run location cannot change yet")
          : undefined}
        onClick={() => {
          close(false);
          if (!options.checked) onSelect(options.target);
        }}
      >
        {options.icon}
        <span className="run-location__label">{options.label}</span>
        {options.hint && <span className="run-location__hint">{options.hint}</span>}
        {envCount(options.envKey) > 0 && (
          <span
            className="run-location__badge"
            title={t("{count} 个环境变量", "{count} environment variables", {
              count: String(envCount(options.envKey))
            })}
          >
            {envCount(options.envKey)}
          </span>
        )}
        {options.checked && <Check size={13} className="run-location__check" />}
      </button>
      <IconButton
        label={options.machine
          ? t("配置机器 {name}", "Configure machine {name}", { name: options.label })
          : t("为 {name} 配置环境变量", "Configure environment variables for {name}", {
            name: options.label
          })}
        className="run-location__gear"
        onClick={() => {
          close(false);
          if (options.machine) setMachineDialog({ machine: options.machine });
          else setEnvDialog({ envKey: options.envKey, name: options.envName });
        }}
      >
        <Settings2 size={13} />
      </IconButton>
    </div>
  );

  return (
    <>
      <button
        type="button"
        ref={triggerRef}
        className={`composer-chip${open ? " popover-menu__trigger--open" : ""}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t("运行地点：{name}", "Run location: {name}", { name: triggerText })}
        onClick={toggle}
      >
        <TriggerIcon size={13} />
        <span className="composer-chip__label">{triggerText}</span>
        <ChevronDown size={11} className="composer-chip__caret" />
      </button>
      {open && createPortal(
        <div
          ref={panelRef}
          tabIndex={-1}
          className={`popover-menu__panel run-location__panel${position?.flipped ? " popover-menu__panel--flipped" : ""}`}
          role="dialog"
          aria-label={t("运行地点", "Run location")}
          style={{
            left: position?.left ?? 0,
            top: position?.top ?? 0,
            width: PANEL_WIDTH,
            visibility: position ? "visible" : "hidden"
          }}
        >
          {row({
            key: "local",
            icon: <Monitor size={14} />,
            label: t("本机", "Local"),
            checked: runTarget === null,
            target: null,
            envKey: "local",
            envName: t("本机", "Local")
          })}
          {distros === null && (
            <p className="run-location__note">{t("正在枚举 WSL 发行版…", "Listing WSL distributions…")}</p>
          )}
          {Boolean(distros?.length) && (
            <>
              <p className="run-location__section">WSL</p>
              {distros!.map((distro) => row({
                key: `wsl:${distro.name}`,
                icon: <SquareTerminal size={14} />,
                label: distro.name,
                hint: distro.isDefault ? t("默认", "default") : undefined,
                checked: runTarget?.kind === "wsl" && runTarget.distro === distro.name,
                target: { kind: "wsl", distro: distro.name },
                envKey: `wsl:${distro.name}`,
                envName: distro.name
              }))}
            </>
          )}
          {sshMachines.length > 0 && (
            <>
              <p className="run-location__section">{t("SSH 机器", "SSH machines")}</p>
              {sshMachines.map((machine) => row({
                key: `ssh:${machine.id}`,
                icon: <Server size={14} />,
                label: machine.name,
                hint: machine.host,
                checked: runTarget?.kind === "ssh" && runTarget.machineId === machine.id,
                target: { kind: "ssh", machineId: machine.id },
                envKey: `ssh:${machine.id}`,
                envName: machine.name,
                machine
              }))}
            </>
          )}
          <button
            type="button"
            className="run-location__add"
            onClick={() => {
              close(false);
              setMachineDialog({ machine: null });
            }}
          >
            <Plus size={13} />
            <span>{t("添加 SSH 机器…", "Add SSH machine…")}</span>
          </button>
        </div>,
        document.body
      )}
      {envDialog && (
        <RunEnvironmentDialog
          title={t("{name} 的环境变量", "Environment variables for {name}", { name: envDialog.name })}
          vars={envVars[envDialog.envKey] ?? {}}
          onSave={(vars) => {
            onSaveEnvVars(envDialog.envKey, vars);
            setEnvDialog(null);
          }}
          onClose={() => setEnvDialog(null)}
        />
      )}
      {machineDialog && (
        <SshMachineDialog
          machine={machineDialog.machine}
          vars={machineDialog.machine ? envVars[`ssh:${machineDialog.machine.id}`] ?? {} : {}}
          isActive={Boolean(machineDialog.machine && activeKey === `ssh:${machineDialog.machine.id}`)}
          machineCount={sshMachines.length}
          onSave={(machine, vars) => {
            onSaveMachine(machine);
            onSaveEnvVars(`ssh:${machine.id}`, vars);
            setMachineDialog(null);
          }}
          onDelete={(machineId) => {
            onDeleteMachine(machineId);
            setMachineDialog(null);
          }}
          onClose={() => setMachineDialog(null)}
        />
      )}
    </>
  );
}

/** Environment-variable editor. The textarea holds its raw draft so incremental
 * edits are not normalized away. */
function RunEnvironmentDialog({
  title,
  vars,
  onSave,
  onClose
}: {
  title: string;
  vars: Record<string, string>;
  onSave: (vars: Record<string, string>) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [text, setText] = useState(() => formatEnvText(vars));
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog
      title={title}
      description={t(
        "每行一条 KEY=value。这些变量会注入这个环境里执行的 shell 命令。",
        "One KEY=value per line. These variables are injected into shell commands run in this environment."
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

/** SSH machine creation and editing dialog, including environment variables. */
function SshMachineDialog({
  machine,
  vars,
  isActive,
  machineCount,
  onSave,
  onDelete,
  onClose
}: {
  machine: SshMachineConfig | null;
  vars: Record<string, string>;
  isActive: boolean;
  /** Registered machine count, used to enforce the host limit during creation. */
  machineCount: number;
  onSave: (machine: SshMachineConfig, vars: Record<string, string>) => void;
  onDelete: (machineId: string) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(machine?.name ?? "");
  const [host, setHost] = useState(machine?.host ?? "");
  const [port, setPort] = useState(machine && machine.port !== 0 ? String(machine.port) : "");
  const [identityFile, setIdentityFile] = useState(machine?.identityFile ?? "");
  const [remoteCwd, setRemoteCwd] = useState(machine?.remoteCwd ?? "");
  const [envText, setEnvText] = useState(() => formatEnvText(vars));
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
        "认证材料不落盘：连接时由 OpenSSH 按身份文件、~/.ssh/config 与 agent 解析。",
        "No credentials are stored: OpenSSH resolves the identity file, ~/.ssh/config and the agent at connect time."
      )}
      width="460px"
      onClose={onClose}
      footer={<>
        {machine && (
          <button
            type="button"
            className="button button--secondary run-location__delete"
            title={isActive
              ? t("这台机器是当前对话的运行地点", "This machine is the current conversation's run location")
              : undefined}
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
            const trimmedCwd = remoteCwd.trim();
            if ([trimmedIdentity, trimmedCwd].some((value) => (
              value.length > MAX_PATH_FIELD_CHARS || CONTROL_CHARS.test(value)
            ))) {
              setError(t(
                "身份文件或远端目录过长或含控制字符（最长 {max}）",
                "The identity file or remote directory is too long or contains control characters (max {max})",
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
            const parsedEnv = parseEnvText(envText);
            const envMessage = envTextError(parsedEnv, t);
            if (envMessage) {
              setError(envMessage);
              return;
            }
            const now = new Date().toISOString();
            onSave({
              id: machine?.id ?? createId("sshm"),
              name: trimmedName,
              host: trimmedHost,
              port: parsedPort,
              identityFile: trimmedIdentity,
              remoteCwd: trimmedCwd,
              createdAt: machine?.createdAt ?? now,
              updatedAt: now
            }, parsedEnv.vars);
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
        {field(t("远端目录（空 = home）", "Remote directory (empty = home)"), remoteCwd, setRemoteCwd, "~/projects/app")}
        <label className="run-location__field">
          <span>{t("环境变量（每行一条 KEY=value）", "Environment variables (one KEY=value per line)")}</span>
          <textarea
            className="run-location__env-input"
            rows={5}
            value={envText}
            spellCheck={false}
            onChange={(event) => {
              setEnvText(event.target.value);
              setError(null);
            }}
          />
        </label>
      </div>
      {error && <p className="run-location__error" role="alert">{error}</p>}
    </Dialog>
  );
}
