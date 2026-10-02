import { Container } from "lucide-react";
import { useState } from "react";
import { useI18n } from "../i18n";
import { workspaceLocationTitle, workspaceMachineLabel } from "../lib/workspaces";
import type { RunTarget, SandboxSettings as SandboxSettingsType, SshMachineConfig } from "../types";
import { Dialog } from "./Common";
import { SandboxSettings } from "./SandboxSettings";
import { SettingsLayout, SettingsNavigation } from "./SettingsLayout";
import { SettingsPageHeading } from "./SettingsPageHeading";
import "./WorkspaceSettings.css";

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

/**
 * A workspace's variables, one `KEY=value` per line. Like every other settings
 * page it applies as it is typed: each draft that parses is saved, and one
 * that does not keeps the last that did. The textarea holds the raw draft so
 * incremental edits are not normalized away, and the draft's problem is shown
 * once the field is left rather than at every keystroke of a half-typed line.
 */
function EnvironmentVariablesCard({
  vars,
  onChange
}: {
  vars: Record<string, string>;
  onChange: (vars: Record<string, string>) => void;
}) {
  const { t } = useI18n();
  const [text, setText] = useState(() => formatEnvText(vars));
  const [problem, setProblem] = useState<string | null>(null);
  const [shown, setShown] = useState(false);
  const label = t("环境变量", "Environment variables");
  return (
    <section className="settings-card workspace-settings__variables">
      {/* Titled by the section heading above it; the field keeps the name for assistive tech. */}
      <div className="workspace-settings__copy">
        <small>{t(
          "每行一条 KEY=value。这些变量会注入在这个工作区里执行的 shell 命令，沙箱里的也一样；名字像密钥的变量进不了沙箱。",
          "One KEY=value per line. These variables are injected into shell commands run in this workspace, sandboxed ones included — though variables named like secrets never reach the sandbox."
        )}</small>
      </div>
      <textarea
        className="run-location__env-input"
        rows={8}
        value={text}
        aria-label={label}
        aria-invalid={shown && problem ? true : undefined}
        placeholder={"API_KEY=value\nHTTP_PROXY=http://127.0.0.1:7890"}
        spellCheck={false}
        onChange={(event) => {
          const next = event.target.value;
          const parsed = parseEnvText(next);
          const message = envTextError(parsed, t);
          setText(next);
          setProblem(message);
          setShown(false);
          if (!message) onChange(parsed.vars);
        }}
        onBlur={() => setShown(true)}
      />
      {shown && problem && (
        <p className="run-location__error" role="alert">
          {t("{problem}。在改好之前，保存的仍是上一版。", "{problem}. Until it is fixed, the last version that was valid stays saved.", {
            problem
          })}
        </p>
      )}
    </section>
  );
}

type WorkspaceSettingsView = "environment";

export interface WorkspaceSettingsDialogProps {
  /** The directory's label, which names the window. */
  name: string;
  /** The machine the workspace is on; `null` is this computer. */
  machine: RunTarget | null;
  /** The directory as the workspace is registered — never a worktree standing in for it. */
  path: string;
  sshMachines: readonly SshMachineConfig[];
  vars: Record<string, string>;
  /** Absent when the workspace's sandbox has never been set: off. */
  sandbox: SandboxSettingsType | undefined;
  onChangeVars: (vars: Record<string, string>) => void;
  onChangeSandbox: (sandbox: SandboxSettingsType) => void;
  onClose: () => void;
}

/**
 * A workspace's settings, opened from the gear beside it: the same window as
 * global settings — a rail of pages under the window's name, the selected page
 * beside it — named after the workspace. It has one page, the environment its
 * commands run in: its sandbox and its variables. Both belong to the
 * workspace, not to its machine or to a conversation, so every conversation
 * working in it gets the same, and other workspaces on the machine are
 * unaffected.
 *
 * Changes apply as they are made, as in global settings; there is nothing to
 * save.
 */
export function WorkspaceSettingsDialog({
  name,
  machine,
  path,
  sshMachines,
  vars,
  sandbox,
  onChangeVars,
  onChangeSandbox,
  onClose
}: WorkspaceSettingsDialogProps) {
  const { t } = useI18n();
  const [view, setView] = useState<WorkspaceSettingsView>("environment");
  const machineName = workspaceMachineLabel(machine, sshMachines) ?? t("这台电脑", "This computer");
  return (
    <Dialog title={name} width="1040px" sidebar bodyClassName="dialog__body--flush" onClose={onClose}>
      <SettingsLayout
        label={t("工作区设置", "Workspace settings")}
        navigation={(
          <SettingsNavigation
            label={t("工作区设置分类", "Workspace settings categories")}
            groups={[{
              id: "pages",
              items: [{ id: "environment", icon: Container, label: t("环境", "Environment") }]
            }]}
            view={view}
            onSelect={setView}
          />
        )}
      >
        {view === "environment" && (
          <section className="settings-page workspace-settings">
            <SettingsPageHeading
              title={t("环境", "Environment")}
              description={t(
                "这个工作区（{location}）里的命令在什么环境里运行：是否进沙箱，带哪些环境变量。同一台机器上的其他工作区不受影响。",
                "How commands run in this workspace ({location}): whether in the sandbox, and with which environment variables. Other workspaces on the same machine are unaffected.",
                { location: workspaceLocationTitle(path, machine, sshMachines) }
              )}
            />
            <h4 className="workspace-settings__section-title">{t("沙箱", "Sandbox")}</h4>
            <SandboxSettings
              machine={machine}
              machineName={machineName}
              settings={sandbox}
              onChange={onChangeSandbox}
            />
            <h4 className="workspace-settings__section-title">{t("环境变量", "Environment variables")}</h4>
            <EnvironmentVariablesCard vars={vars} onChange={onChangeVars} />
          </section>
        )}
      </SettingsLayout>
    </Dialog>
  );
}
