import { Info } from "lucide-react";
import type { JSX } from "react";
import { useI18n } from "../../i18n";
import type { McpServerConfig, McpTransportKind } from "../../types";
import { Field, Switch } from "../Common";

/**
 * General MCP server form.
 *
 * Changes write through immediately; the footer is reserved for connection testing
 * and deletion rather than a separate save action.
 */

/** Package-registry mirror option. A `custom` URL requires an additional address. */
interface RegistryOption {
  url: string;
  label: string;
}

/** Command-to-mirror mapping aligned with Rust `mcp.rs::registry_mirror_variables`. */
export function registryOptionsFor(command: string, t: ReturnType<typeof useI18n>["t"]): RegistryOption[] | null {
  const executable = command.trim().split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const stem = executable.replace(/\.(exe|cmd|bat)$/, "");
  if (["npx", "npm", "bun", "bunx", "pnpm", "yarn"].includes(stem)) {
    return [
      { url: "https://registry.npmmirror.com", label: t("淘宝 NPM Mirror", "Taobao NPM Mirror") },
      { url: "custom", label: t("自定义", "Custom") }
    ];
  }
  if (["uv", "uvx", "pip", "pipx", "python", "python3"].includes(stem)) {
    return [
      { url: "https://pypi.tuna.tsinghua.edu.cn/simple", label: t("清华大学", "Tsinghua University") },
      { url: "http://mirrors.aliyun.com/pypi/simple/", label: t("阿里云", "Aliyun") },
      { url: "https://mirrors.ustc.edu.cn/pypi/simple/", label: t("中国科学技术大学", "USTC") },
      { url: "https://repo.huaweicloud.com/repository/pypi/simple/", label: t("华为云", "Huawei Cloud") },
      { url: "https://mirrors.cloud.tencent.com/pypi/simple/", label: t("腾讯云", "Tencent Cloud") },
      { url: "custom", label: t("自定义", "Custom") }
    ];
  }
  return null;
}

function Hint({ text }: { text: string }): JSX.Element {
  return (
    <span className="mcp-hint" title={text}>
      <Info size={12} aria-hidden="true" />
      <span className="mcp-hint__text">{text}</span>
    </span>
  );
}

export function formatRecord(value: Record<string, string>, separator: string): string {
  return Object.entries(value).map(([key, item]) => `${key}${separator}${item}`).join("\n");
}

export function parseRecord(value: string, separator: string): Record<string, string> {
  const result: Record<string, string> = {};
  for (const line of value.split(/\r?\n/)) {
    const position = line.indexOf(separator);
    if (position < 0) continue;
    const key = line.slice(0, position).trim();
    if (!key) continue;
    result[key] = line.slice(position + separator.length).trim();
  }
  return result;
}

export function parseLines(value: string): string[] {
  return value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
}

/** Uncommitted textarea input. Without it, an unfinished empty line would be normalized away. */
export interface TextDraft {
  args?: string;
  env?: string;
  headers?: string;
}

export function McpServerForm({
  server,
  draft,
  onDraftChange,
  onChange,
  singleColumn = false
}: {
  server: McpServerConfig;
  draft: TextDraft;
  onDraftChange: (draft: TextDraft) => void;
  onChange: (update: (server: McpServerConfig) => McpServerConfig) => void;
  /** The quick-create dialog uses one column; the detail view uses two. */
  singleColumn?: boolean;
}): JSX.Element {
  const { t } = useI18n();
  const registryOptions = server.transport === "stdio"
    ? registryOptionsFor(server.command, t)
    : null;
  const presetUrls = new Set((registryOptions ?? []).map((option) => option.url));
  const custom = Boolean(server.registryUrl) && !presetUrls.has(server.registryUrl);
  const gridClass = singleColumn ? "mcp-form-grid mcp-form-grid--single" : "mcp-form-grid";

  return (
    <>
      <section className={gridClass}>
        <Field label={t("名称", "Name")}>
          <input
            className="input"
            required
            value={server.name}
            onChange={(event) => onChange((current) => ({ ...current, name: event.target.value }))}
          />
        </Field>
        <Field label={t("类型", "Type")}>
          <select
            className="input"
            value={server.transport}
            onChange={(event) => onChange((current) => ({
              ...current,
              transport: event.target.value as McpTransportKind
            }))}
          >
            <option value="stdio">{t("标准输入 / 输出 (stdio)", "Standard input/output (stdio)")}</option>
            <option value="streamable_http">{t("可流式传输的 HTTP (streamableHttp)", "Streamable HTTP (streamableHttp)")}</option>
          </select>
        </Field>
        <div className="mcp-form-grid__span">
          <Field label={t("描述", "Description")}>
            <textarea
              className="input mcp-textarea"
              rows={2}
              value={server.description}
              onChange={(event) => onChange((current) => ({ ...current, description: event.target.value }))}
            />
          </Field>
        </div>
      </section>

      <section className={gridClass}>
        {server.transport === "stdio" ? (
          <Field label={t("命令", "Command")}>
            <input
              className="input"
              value={server.command}
              placeholder={t("uvx 或 npx", "uvx or npx")}
              onChange={(event) => {
                const command = event.target.value;
                onChange((current) => {
                  // Clear an incompatible mirror when the command switches package ecosystems.
                  const stillApplies = registryOptionsFor(command, t)?.some(
                    (option) => option.url === current.registryUrl
                  ) ?? false;
                  return {
                    ...current,
                    command,
                    registryUrl: stillApplies || custom ? current.registryUrl : ""
                  };
                });
              }}
            />
          </Field>
        ) : (
          <Field label={t("URL", "URL")}>
            <input
              className="input"
              type="url"
              value={server.url}
              title={t("远程 URL 地址", "Remote URL")}
              placeholder="http://localhost:3000/mcp"
              onChange={(event) => onChange((current) => ({ ...current, url: event.target.value }))}
            />
          </Field>
        )}

        {server.transport === "streamable_http" && (
          <Field label={t("请求头", "Headers")} hint={t("HTTP 请求的自定义请求头，每行一个 Name: value", "Custom headers for HTTP requests, one Name: value per line")}>
            <textarea
              className="input mcp-code-textarea"
              rows={3}
              placeholder={"Content-Type: application/json\nAuthorization: Bearer token"}
              value={draft.headers ?? formatRecord(server.headers, ": ")}
              onChange={(event) => {
                const value = event.target.value;
                onDraftChange({ ...draft, headers: value });
                onChange((current) => ({ ...current, headers: parseRecord(value, ":") }));
              }}
            />
          </Field>
        )}

        {server.transport === "stdio" && registryOptions && (
          <div className="mcp-form-grid__span">
            <fieldset className="mcp-registry">
              <legend>
                {t("包管理源", "Package registry")}
                <Hint text={t("选择用于安装包的源，以解决默认源的网络问题", "Pick a mirror when the default registry is unreachable")} />
              </legend>
              <div className="mcp-registry__options">
                <label className="mcp-radio">
                  <input
                    type="radio"
                    name={`registry-${server.id}`}
                    checked={!server.registryUrl}
                    onChange={() => onChange((current) => ({ ...current, registryUrl: "" }))}
                  />
                  <span>{t("默认", "Default")}</span>
                </label>
                {registryOptions.map((option) => (
                  <label className="mcp-radio" key={option.url}>
                    <input
                      type="radio"
                      name={`registry-${server.id}`}
                      checked={option.url === "custom" ? custom : server.registryUrl === option.url}
                      onChange={() => onChange((current) => ({
                        ...current,
                        registryUrl: option.url === "custom" ? current.registryUrl : option.url
                      }))}
                    />
                    <span>{option.label}</span>
                  </label>
                ))}
              </div>
              {custom && (
                <input
                  className="input mcp-registry__custom"
                  value={server.registryUrl}
                  aria-label={t("自定义包管理源地址", "Custom registry URL")}
                  placeholder={t("请输入私有仓库地址，如: https://npm.company.com", "Private registry URL, e.g. https://npm.company.com")}
                  onChange={(event) => onChange((current) => ({ ...current, registryUrl: event.target.value }))}
                />
              )}
            </fieldset>
          </div>
        )}

        {server.transport === "stdio" && (
          <>
            <Field label={t("参数", "Arguments")} hint={t("每个参数占一行", "One argument per line")}>
              <textarea
                className="input mcp-code-textarea"
                rows={3}
                placeholder={"arg1\narg2"}
                value={draft.args ?? server.args.join("\n")}
                onChange={(event) => {
                  const value = event.target.value;
                  onDraftChange({ ...draft, args: value });
                  onChange((current) => ({ ...current, args: parseLines(value) }));
                }}
              />
            </Field>
            <Field label={t("环境变量", "Environment variables")} hint={t("格式：KEY=value，每行一个", "One KEY=value per line")}>
              <textarea
                className="input mcp-code-textarea"
                rows={3}
                placeholder={"KEY1=value1\nKEY2=value2"}
                value={draft.env ?? formatRecord(server.env, "=")}
                onChange={(event) => {
                  const value = event.target.value;
                  onDraftChange({ ...draft, env: value });
                  onChange((current) => ({ ...current, env: parseRecord(value, "=") }));
                }}
              />
            </Field>
          </>
        )}
      </section>

      <section className={gridClass}>
        <div className="mcp-inline-card">
          <span>
            {t("长时间运行模式", "Long-running")}
            <Hint text={t("启用后，未单独设置超时的调用按 5 分钟上限等待，而不是默认的 45 秒", "When enabled, calls without an explicit timeout wait up to the 5-minute ceiling instead of the 45-second default")} />
          </span>
          <Switch
            checked={server.longRunning}
            onChange={(longRunning) => onChange((current) => ({ ...current, longRunning }))}
            label={t("长时间运行模式", "Long-running")}
          />
        </div>
        <div className="mcp-inline-card">
          <span>
            {t("超时", "Timeout")}
            <Hint text={t("对该服务器请求的超时时间（秒），0 表示使用默认值", "Request timeout in seconds; 0 uses the default")} />
          </span>
          <span className="mcp-inline-card__value">
            <input
              className="input"
              type="number"
              min={0}
              step={1}
              aria-label={t("超时（秒）", "Timeout (seconds)")}
              value={server.timeoutSeconds}
              onChange={(event) => {
                const timeoutSeconds = Math.max(0, Math.trunc(Number(event.target.value) || 0));
                onChange((current) => ({ ...current, timeoutSeconds }));
              }}
            />
            <em>{t("秒", "s")}</em>
          </span>
        </div>
      </section>
    </>
  );
}
