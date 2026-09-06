import { ChevronRight, Clipboard, FileText, MessageSquareQuote, Wrench } from "lucide-react";
import type { JSX } from "react";
import { useMemo, useState } from "react";
import { useI18n } from "../../i18n";
import type {
  JsonValue,
  McpProbePrompt,
  McpProbeResource,
  McpProbeTool
} from "../../types";
import { IconButton, Switch } from "../Common";

function isRecord(value: unknown): value is Record<string, JsonValue> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function schemaType(schema: Record<string, JsonValue>): string {
  if (typeof schema.type === "string") {
    if (schema.type === "array" && isRecord(schema.items) && typeof schema.items.type === "string") {
      return `${schema.items.type}[]`;
    }
    return schema.type;
  }
  if (isRecord(schema.properties)) return "object";
  return "unknown";
}

/** Input schema tree. Stop after five nested levels; deeper schemas cannot be understood at a glance. */
const MAX_SCHEMA_DEPTH = 5;

function SchemaProperties({ schema, depth = 0 }: { schema: JsonValue; depth?: number }): JSX.Element {
  const { t } = useI18n();
  if (!isRecord(schema) || !isRecord(schema.properties)) {
    return <p className="mcp-schema__empty">{t("未声明输入字段", "No input properties declared")}</p>;
  }
  const required = new Set(Array.isArray(schema.required)
    ? schema.required.filter((item): item is string => typeof item === "string")
    : []);
  const properties = Object.entries(schema.properties);
  if (!properties.length) {
    return <p className="mcp-schema__empty">{t("未声明输入字段", "No input properties declared")}</p>;
  }

  return (
    <div className={depth === 0 ? "mcp-schema" : "mcp-schema mcp-schema--nested"}>
      {properties.map(([name, value]) => {
        const property = isRecord(value) ? value : {};
        const type = schemaType(property);
        const enumValues = Array.isArray(property.enum) ? property.enum : null;
        const nested = depth < MAX_SCHEMA_DEPTH && isRecord(property.properties)
          ? property
          : depth < MAX_SCHEMA_DEPTH && type.endsWith("[]") && isRecord(property.items) && isRecord(property.items.properties)
            ? property.items
            : null;
        return (
          <div className="mcp-schema__row" key={`${depth}-${name}`}>
            <div className="mcp-schema__line">
              <code>{name}</code>
              {required.has(name) && <em title={t("必填", "Required")}>*</em>}
              <span className={`mcp-schema__type mcp-schema__type--${type.replace("[]", "")}`}>{type}</span>
              {typeof property.description === "string" && property.description && (
                <small>{property.description}</small>
              )}
            </div>
            {enumValues && enumValues.length > 0 && (
              <div className="mcp-schema__enum">
                <span>{t("允许的值", "Allowed values")}</span>
                {enumValues.map((item) => (
                  <em key={String(item)}>{String(item)}</em>
                ))}
              </div>
            )}
            {nested && <SchemaProperties schema={nested as JsonValue} depth={depth + 1} />}
          </div>
        );
      })}
    </div>
  );
}

export function McpToolsSection({
  tools,
  search,
  disabledTools,
  disabledAutoApproveTools,
  onToggleTool,
  onToggleAutoApprove
}: {
  tools: McpProbeTool[];
  search: string;
  disabledTools: string[];
  disabledAutoApproveTools: string[];
  onToggleTool: (name: string, enabled: boolean) => void;
  onToggleAutoApprove: (name: string, enabled: boolean) => void;
}): JSX.Element {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const needle = search.trim().toLowerCase();
  const visible = useMemo(() => (needle
    ? tools.filter((tool) => tool.name.toLowerCase().includes(needle)
      || tool.title.toLowerCase().includes(needle)
      || tool.description.toLowerCase().includes(needle))
    : tools), [needle, tools]);

  if (!visible.length) {
    return (
      <div className="mcp-empty">
        <Wrench size={20} />
        <strong>{needle ? t("没有匹配结果", "No results") : t("无可用工具", "No tools available")}</strong>
      </div>
    );
  }

  return (
    <div className="mcp-table">
      {/* The header is three column labels rather than an ARIA table: its latter two columns
          are independently labelled switches, and extra grid/table semantics would announce each twice. */}
      <div className="mcp-table__head" aria-hidden="true">
        <span>{t("可用工具", "Available tools")}</span>
        <span>{t("启用工具", "Enable")}</span>
        <span>{t("自动批准", "Auto approve")}</span>
      </div>
      {visible.map((tool) => {
        const enabled = !disabledTools.includes(tool.name);
        const autoApprove = !disabledAutoApproveTools.includes(tool.name);
        const open = expanded.has(tool.name);
        const expandable = Boolean(tool.description) || isRecord(tool.inputSchema);
        return (
          <div className="mcp-table__group" key={tool.name}>
            <div className="mcp-table__row">
              <div className="mcp-table__cell mcp-table__cell--main">
                <button
                  type="button"
                  className="mcp-table__expand"
                  aria-expanded={open}
                  disabled={!expandable}
                  onClick={() => setExpanded((current) => {
                    const next = new Set(current);
                    if (next.has(tool.name)) next.delete(tool.name);
                    else next.add(tool.name);
                    return next;
                  })}
                >
                  <ChevronRight
                    size={13}
                    className={open ? "disclosure-chevron disclosure-chevron--open" : "disclosure-chevron"}
                  />
                  <span>
                    <strong title={tool.name}>{tool.name}</strong>
                    {tool.description && <small>{tool.description}</small>}
                  </span>
                </button>
              </div>
              <div className="mcp-table__cell mcp-table__cell--switch">
                <Switch
                  checked={enabled}
                  onChange={(next) => onToggleTool(tool.name, next)}
                  label={t("启用工具 {name}", "Enable tool {name}", { name: tool.name })}
                />
              </div>
              <div className="mcp-table__cell mcp-table__cell--switch">
                <Switch
                  checked={autoApprove}
                  disabled={!enabled}
                  onChange={(next) => onToggleAutoApprove(tool.name, next)}
                  label={t("自动批准工具 {name}", "Auto approve tool {name}", { name: tool.name })}
                />
              </div>
            </div>
            {open && (
              <div className="mcp-table__expanded">
                {tool.description && (
                  <>
                    <h4>{t("描述", "Description")}</h4>
                    <p>{tool.description}</p>
                  </>
                )}
                <h4>{t("输入模式", "Input schema")}</h4>
                <SchemaProperties schema={tool.inputSchema} />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function McpPromptsSection({ prompts }: { prompts: McpProbePrompt[] }): JSX.Element {
  const { t } = useI18n();
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  if (!prompts.length) {
    return (
      <div className="mcp-empty">
        <MessageSquareQuote size={20} />
        <strong>{t("无可用提示", "No prompts available")}</strong>
      </div>
    );
  }
  return (
    <div className="mcp-accordion">
      <h3>{t("可用提示", "Available prompts")}</h3>
      {prompts.map((prompt) => {
        const expanded = open.has(prompt.name);
        return (
          <div className="mcp-accordion__item" key={prompt.name}>
            <button
              type="button"
              className="mcp-accordion__trigger"
              aria-expanded={expanded}
              onClick={() => setOpen((current) => {
                const next = new Set(current);
                if (next.has(prompt.name)) next.delete(prompt.name);
                else next.add(prompt.name);
                return next;
              })}
            >
              <ChevronRight
                size={13}
                className={expanded ? "disclosure-chevron disclosure-chevron--open" : "disclosure-chevron"}
              />
              <span>
                <strong>{prompt.title || prompt.name}</strong>
                {prompt.description && <small>{prompt.description}</small>}
              </span>
            </button>
            {expanded && (
              <div className="mcp-accordion__body">
                {prompt.arguments.length ? (
                  <dl className="mcp-detail-list">
                    {prompt.arguments.map((argument) => (
                      <div key={argument.name}>
                        <dt>
                          {argument.name}
                          {argument.required && <em title={t("必填", "Required")}>*</em>}
                        </dt>
                        <dd>{argument.description || t("没有描述", "No description")}</dd>
                      </div>
                    ))}
                  </dl>
                ) : (
                  <p className="mcp-schema__empty">{t("未声明参数", "No arguments declared")}</p>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function formatFileSize(size: number, unknown: string): string {
  if (!size) return unknown;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(2)} ${units[index]}`;
}

export function McpResourcesSection({ resources }: { resources: McpProbeResource[] }): JSX.Element {
  const { t } = useI18n();
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  if (!resources.length) {
    return (
      <div className="mcp-empty">
        <FileText size={20} />
        <strong>{t("无可用资源", "No resources available")}</strong>
      </div>
    );
  }
  return (
    <div className="mcp-accordion">
      <h3>{t("可用资源", "Available resources")}</h3>
      {resources.map((resource) => {
        const expanded = open.has(resource.uri);
        return (
          <div className="mcp-accordion__item" key={resource.uri}>
            <button
              type="button"
              className="mcp-accordion__trigger"
              aria-expanded={expanded}
              onClick={() => setOpen((current) => {
                const next = new Set(current);
                if (next.has(resource.uri)) next.delete(resource.uri);
                else next.add(resource.uri);
                return next;
              })}
            >
              <ChevronRight
                size={13}
                className={expanded ? "disclosure-chevron disclosure-chevron--open" : "disclosure-chevron"}
              />
              <span>
                <strong>{`${resource.title || resource.name} (${resource.uri})`}</strong>
                {resource.description && <small>{resource.description}</small>}
              </span>
            </button>
            {expanded && (
              <div className="mcp-accordion__body">
                <dl className="mcp-detail-list">
                  <div>
                    <dt>{t("MIME 类型", "MIME type")}</dt>
                    <dd>{resource.mimeType || t("未申报", "Not declared")}</dd>
                  </div>
                  <div>
                    <dt>{t("大小", "Size")}</dt>
                    <dd>{formatFileSize(resource.size, t("未申报", "Not declared"))}</dd>
                  </div>
                </dl>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function McpLogsSection({ logs }: { logs: string[] }): JSX.Element {
  const { t } = useI18n();
  return (
    <div className="mcp-logs">
      <div className="mcp-logs__toolbar">
        <span>{t("MCP 服务器进程的日志", "Logs from the MCP server process")}</span>
        <IconButton
          label={t("复制日志", "Copy logs")}
          disabled={!logs.length}
          onClick={() => void navigator.clipboard?.writeText(logs.join("\n"))}
        ><Clipboard size={14} /></IconButton>
      </div>
      {logs.length ? (
        <pre className="mcp-logs__body">{logs.join("\n")}</pre>
      ) : (
        <div className="mcp-empty">
          <FileText size={20} />
          <strong>{t("暂无日志", "No logs yet")}</strong>
          <small>{t("stdio 服务器写到 stderr 的内容会在测试连接后出现在这里。", "Whatever a stdio server writes to stderr shows up here after a connection test.")}</small>
        </div>
      )}
    </div>
  );
}
