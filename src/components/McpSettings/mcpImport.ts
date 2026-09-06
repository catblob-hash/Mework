import { createId } from "../../lib/id";
import type { TranslationFunction } from "../../i18n";
import type { McpServerConfig, McpTransportKind } from "../../types";

/** An import entry that cannot be imported. `entry` is its JSON key. */
export interface McpImportError {
  entry: string;
  message: string;
}

interface DuplicateKey {
  path: string[];
  key: string;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * `JSON.parse` silently discards duplicate object keys, which are MCP server names.
 * Scan the parsed structure for duplicate keys so an import containing repeated
 * server names is rejected instead of silently retaining only the last one.
 */
function collectDuplicateKeys(source: string): DuplicateKey[] {
  const duplicates: DuplicateKey[] = [];
  let index = 0;

  const skipSpace = () => {
    while (/\s/.test(source[index] ?? "")) index += 1;
  };

  const readString = (): string => {
    const start = index;
    index += 1;
    while (index < source.length) {
      if (source[index] === "\\") {
        index += 2;
        continue;
      }
      if (source[index] === "\"") {
        index += 1;
        break;
      }
      index += 1;
    }
    const parsed: unknown = JSON.parse(source.slice(start, index));
    return typeof parsed === "string" ? parsed : "";
  };

  const visitValue = (path: string[]) => {
    skipSpace();
    if (source[index] === "{") {
      visitObject(path);
      return;
    }
    if (source[index] === "[") {
      index += 1;
      skipSpace();
      while (index < source.length && source[index] !== "]") {
        visitValue(path);
        skipSpace();
        if (source[index] === ",") {
          index += 1;
          skipSpace();
        }
      }
      if (source[index] === "]") index += 1;
      return;
    }
    if (source[index] === "\"") {
      readString();
      return;
    }
    while (index < source.length && !/[\s,}\]]/.test(source[index])) index += 1;
  };

  const visitObject = (path: string[]) => {
    index += 1;
    const seen = new Set<string>();
    skipSpace();
    while (index < source.length && source[index] !== "}") {
      const key = readString();
      if (seen.has(key)) duplicates.push({ path, key });
      seen.add(key);
      skipSpace();
      if (source[index] === ":") index += 1;
      visitValue([...path, key]);
      skipSpace();
      if (source[index] === ",") {
        index += 1;
        skipSpace();
      }
    }
    if (source[index] === "}") index += 1;
  };

  visitValue([]);
  return duplicates;
}

function stringRecord(value: unknown): Record<string, string> | null {
  if (value === undefined) return {};
  if (!isRecord(value)) return null;
  const entries = Object.entries(value);
  if (entries.some(([, item]) => typeof item !== "string")) return null;
  return Object.fromEntries(entries) as Record<string, string>;
}

function stringArray(value: unknown): string[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) return null;
  return value as string[];
}

function importServer(
  name: string,
  raw: unknown,
  sortOrder: number,
  now: string,
  t: TranslationFunction
): { server?: McpServerConfig; error?: string } {
  if (!isRecord(raw)) return { error: t("服务器配置必须是对象", "Server configuration must be an object") };
  const normalizedName = name.trim();
  if (!normalizedName) return { error: t("服务器名称不能为空", "Server name is required") };

  const rawType = typeof raw.type === "string" ? raw.type.replaceAll("-", "_").toLowerCase() : "";
  const httpTypes = new Set(["streamablehttp", "streamable_http", "http"]);
  if (rawType && rawType !== "stdio" && !httpTypes.has(rawType)) {
    return { error: t("不支持的服务器类型：{type}", "Unsupported server type: {type}", { type: rawType }) };
  }
  const transport: McpTransportKind = httpTypes.has(rawType) || typeof raw.url === "string"
    ? "streamable_http"
    : "stdio";
  const args = stringArray(raw.args);
  const env = stringRecord(raw.env);
  const headers = stringRecord(raw.headers);
  if (!args) return { error: t("参数必须是字符串数组", "Args must be an array of strings") };
  if (!env) return { error: t("环境变量必须是字符串键值对象", "Environment variables must be a string map") };
  if (!headers) return { error: t("请求头必须是字符串键值对象", "Headers must be a string map") };

  const command = typeof raw.command === "string" ? raw.command.trim() : "";
  const url = typeof raw.url === "string" ? raw.url.trim() : "";
  if (transport === "stdio" && !command) {
    return { error: t("STDIO 服务器需要启动命令", "STDIO servers require a command") };
  }
  if (transport === "streamable_http") {
    try {
      const parsedUrl = new URL(url);
      if (parsedUrl.protocol !== "http:" && parsedUrl.protocol !== "https:") throw new Error("protocol");
    } catch {
      return { error: t("Streamable HTTP 服务器需要有效的 HTTP(S) 地址", "Streamable HTTP servers require a valid HTTP(S) URL") };
    }
  }

  const timeout = typeof raw.timeoutSeconds === "number" && Number.isFinite(raw.timeoutSeconds)
    ? Math.max(0, Math.trunc(raw.timeoutSeconds))
    : 0;
  const disabledTools = stringArray(raw.disabledTools);
  const disabledAutoApproveTools = stringArray(raw.disabledAutoApproveTools);
  if (!disabledTools || !disabledAutoApproveTools) {
    return { error: t("工具排除项必须是字符串数组", "Tool exclusions must be arrays of strings") };
  }

  return {
    server: {
      id: createId("mcp_server"),
      name: normalizedName,
      description: typeof raw.description === "string" ? raw.description : "",
      enabled: typeof raw.enabled === "boolean" ? raw.enabled : true,
      transport,
      command,
      args,
      env,
      registryUrl: typeof raw.registryUrl === "string" ? raw.registryUrl.trim() : "",
      url,
      headers,
      timeoutSeconds: timeout,
      longRunning: typeof raw.longRunning === "boolean" ? raw.longRunning : false,
      provider: typeof raw.provider === "string" ? raw.provider : "",
      providerUrl: typeof raw.providerUrl === "string" ? raw.providerUrl : "",
      tags: stringArray(raw.tags) ?? [],
      disabledTools,
      disabledAutoApproveTools,
      sortOrder,
      createdAt: now,
      updatedAt: now
    }
  };
}

/** A blank server. */
export function makeServer(sortOrder: number, name: string): McpServerConfig {
  const now = new Date().toISOString();
  return {
    id: createId("mcp_server"),
    name,
    description: "",
    enabled: false,
    transport: "stdio",
    command: "",
    args: [],
    env: {},
    registryUrl: "",
    url: "",
    headers: {},
    timeoutSeconds: 0,
    longRunning: false,
    provider: "",
    providerUrl: "",
    tags: [],
    disabledTools: [],
    disabledAutoApproveTools: [],
    sortOrder,
    createdAt: now,
    updatedAt: now
  };
}

/**
 * Parse an `mcpServers` JSON document.
 *
 * Imports are atomic: reject the entire batch when any entry fails, rather than
 * leaving users to reconcile which servers were imported from a copied document.
 */
export function parseServerImport(
  text: string,
  existing: McpServerConfig[],
  t: TranslationFunction
): { servers: McpServerConfig[]; errors: McpImportError[] } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text) as unknown;
  } catch {
    return { servers: [], errors: [{ entry: t("JSON", "JSON"), message: t("JSON 格式无效", "Invalid JSON") }] };
  }
  if (!isRecord(parsed)) {
    return {
      servers: [],
      errors: [{ entry: t("JSON", "JSON"), message: t("顶层必须是服务器对象", "The top level must be a server map") }]
    };
  }
  const wrapped = Object.hasOwn(parsed, "mcpServers");
  const entriesValue = wrapped ? parsed.mcpServers : parsed;
  if (!isRecord(entriesValue)) {
    return {
      servers: [],
      errors: [{ entry: t("mcpServers", "mcpServers"), message: t("mcpServers 必须是对象", "mcpServers must be an object") }]
    };
  }

  const errors: McpImportError[] = [];
  const duplicatePath = wrapped ? ["mcpServers"] : [];
  for (const duplicate of collectDuplicateKeys(text)) {
    if (duplicate.path.length === duplicatePath.length
      && duplicate.path.every((part, index) => part === duplicatePath[index])) {
      errors.push({
        entry: duplicate.key,
        message: t("同一份导入数据中名称重复", "Duplicate name within this import")
      });
    }
  }

  const existingNames = new Set(existing.map((server) => server.name.trim().toLowerCase()));
  const batchNames = new Set<string>();
  const now = new Date().toISOString();
  const baseSortOrder = Math.max(-1, ...existing.map((server) => server.sortOrder)) + 1;
  const imported: McpServerConfig[] = [];
  for (const [entryName, value] of Object.entries(entriesValue)) {
    const normalized = entryName.trim().toLowerCase();
    if (existingNames.has(normalized)) {
      errors.push({ entry: entryName, message: t("名称已存在", "Name already exists") });
      continue;
    }
    if (batchNames.has(normalized)) {
      errors.push({ entry: entryName, message: t("同一份导入数据中名称重复", "Duplicate name within this import") });
      continue;
    }
    batchNames.add(normalized);
    const result = importServer(entryName, value, baseSortOrder + imported.length, now, t);
    if (result.error) errors.push({ entry: entryName, message: result.error });
    else if (result.server) imported.push(result.server);
  }
  if (!Object.keys(entriesValue).length) {
    errors.push({ entry: t("JSON", "JSON"), message: t("没有可导入的服务器", "No servers to import") });
  }
  return { servers: errors.length ? [] : imported, errors };
}
