import type {
  ApiProvider,
  AppDocument,
  ContextItem,
  Conversation,
  ConversationWebSearchSettings,
  ModelProfile,
  ModelRunRequest,
  ModelRunResponse,
  ModelStreamEvent,
  ToolContext,
  WebSearchAssets,
  Workspace,
} from "./types";
import { Channel, hasBackendRuntime, invoke } from "./lib/backend";
import {
  defaultConversationWebSearchSettings,
  flushDocumentSaves,
  loadDocument,
  saveApiKey,
  saveDocument,
} from "./lib/runtime";
import type { BrowserCloseDisposition, BrowserStatus } from "./lib/browser";
import { issueBrowserLifecycleIntentEpoch } from "./lib/browserLifecycleIntent";
import { createE2eConversation } from "./lib/e2eConversationFixture";

type JsonObject = Record<string, unknown>;
type CheckState = "PASS" | "FAIL" | "BLOCKED";

interface CheckResult {
  name: string;
  state: CheckState;
  detail: string;
}

interface ReloadState {
  version: 9;
  runId: string;
  /** The OpenAI-Responses provider; also the one whose fake key cleanup asserts on. */
  provider: ApiProvider;
  /** The Anthropic-Messages provider the native `web_search_20250305` legs run on. */
  anthropicProvider: ApiProvider;
  conversationIds: string[];
  /** Conversations that actually ran a model turn and therefore persisted receipts. */
  turnConversationIds: string[];
  expectedContextIds: Record<string, string[]>;
  expectedContextDigests: Record<string, string>;
  originalWebSearch: WebSearchAssets;
  originalActiveProviderId: string | null;
  checks: CheckResult[];
}

interface ReportPayload {
  status: "passed" | "failed" | "blocked";
  checks: CheckResult[];
  failure?: string;
}

interface RestartAcceptance {
  accepted?: boolean;
  instanceId?: string;
}

interface DebugRequestEvent {
  type: "debug_request_body";
  round: number;
  body: unknown;
}

const MODEL_API_KEY = "MEWORK_E2E_FAKE_MODEL_KEY_91d8e2c4_not_real";
const COOKIE_VALUE = "fixture-only";
const RELOAD_STATE_KEY = "mework.web-search-e2e.reload.v8";
const LEGACY_RELOAD_STATE_KEYS = [
  "mework.web-search-e2e.reload.v1",
  "mework.web-search-e2e.reload.v2",
  "mework.web-search-e2e.reload.v3",
  "mework.web-search-e2e.reload.v4",
  "mework.web-search-e2e.reload.v5",
  "mework.web-search-e2e.reload.v6",
  "mework.web-search-e2e.reload.v7",
];
const protocolBaseUrl =
  import.meta.env.VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL?.trim() ?? "";
const browserOrigin =
  import.meta.env.VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN?.trim() ?? "";
const configuredRunId =
  import.meta.env.VITE_WEB_SEARCH_E2E_RUN_ID?.trim() ?? "";
const reportUrl = import.meta.env.VITE_WEB_SEARCH_E2E_REPORT_URL?.trim() ?? "";
const reportToken =
  import.meta.env.VITE_WEB_SEARCH_E2E_REPORT_TOKEN?.trim() ?? "";
const results = document.querySelector<HTMLOListElement>("#results")!;
const summary = document.querySelector<HTMLOutputElement>("#summary")!;
const failure = document.querySelector<HTMLPreElement>("#failure")!;
const checks: CheckResult[] = [];

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function object(value: unknown, label: string): JsonObject {
  assert(
    value !== null && typeof value === "object" && !Array.isArray(value),
    `${label} 必须是对象`,
  );
  return value as JsonObject;
}

function clone<T>(value: T): T {
  return typeof structuredClone === "function"
    ? structuredClone(value)
    : (JSON.parse(JSON.stringify(value)) as T);
}

function createId(prefix: string): string {
  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 20);
  return `${prefix}-${suffix}`;
}

function record(name: string, state: CheckState, detail: string): void {
  const result = { name, state, detail };
  checks.push(result);
  renderCheck(result);
}

function renderCheck(result: CheckResult): void {
  const item = document.createElement("li");
  const name = document.createElement("code");
  const status = document.createElement("strong");
  const copy = document.createElement("span");
  name.textContent = result.name;
  status.textContent = result.state;
  status.className = result.state.toLowerCase();
  copy.textContent = result.detail;
  item.append(name, status, copy);
  results.append(item);
}

function isLoopbackHttpOrigin(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      url.protocol === "http:" &&
      url.pathname === "/" &&
      !url.search &&
      !url.hash &&
      (url.hostname === "127.0.0.1" || url.hostname === "[::1]")
    );
  } catch {
    return false;
  }
}

function isLoopbackProtocolBase(value: string): boolean {
  try {
    const url = new URL(value);
    return (
      url.protocol === "http:" &&
      (url.hostname === "127.0.0.1" || url.hostname === "[::1]") &&
      url.pathname === "/v1" &&
      !url.search &&
      !url.hash
    );
  } catch {
    return false;
  }
}

function knownSensitiveValues(): string[] {
  return [MODEL_API_KEY, COOKIE_VALUE];
}

function assertNoSensitiveValues(value: unknown, label: string): void {
  const serialized = typeof value === "string" ? value : JSON.stringify(value);
  for (const sensitive of knownSensitiveValues()) {
    assert(
      !serialized.includes(sensitive),
      `${label} 泄露了已知合成凭据/Cookie 值`,
    );
  }
}

function temporaryWorkspace(documentValue: AppDocument): Workspace {
  const workspace = documentValue.workspaces.find(
    (candidate) => candidate.kind === "temporary",
  );
  assert(workspace, "隔离文档缺少临时工作区");
  return workspace;
}

function makeConversation(
  id: string,
  title: string,
  selection: ConversationWebSearchSettings["provider"],
  maxSearchesPerCall: number,
): Conversation {
  return createE2eConversation({
    id,
    title,
    settings: {
      enabledTools: ["web_search"],
      // Memory tools are derived from the two tier switches, not listed in
      // `enabledTools`. Leaving either tier on would put its three memory tools in
      // the main request and break the harness's exact-allowlist assertion.
      globalMemoryEnabled: false,
      projectMemoryEnabled: false,
      hookIds: [],
      skillIds: [],
      mcpIds: [],
      // Search behaviour is conversation-local: which backend runs the search and
      // how many searches it may make. A catalog provider row itself (endpoint,
      // model, key) is a global asset; `native` uses the conversation's own model.
      webSearch: {
        ...defaultConversationWebSearchSettings(),
        maxSearchesPerCall,
        provider: selection,
      },
      reasoningEffort: "disabled",
      securityLevel: "full_access",
    },
  });
}

function findConversation(
  documentValue: AppDocument,
  conversationId: string,
): Conversation {
  const conversation = documentValue.workspaces
    .flatMap((workspace) => workspace.conversations)
    .find((candidate) => candidate.id === conversationId);
  assert(conversation, `找不到 E2E 对话 ${conversationId}`);
  return conversation;
}

/** Two providers point to the same protocol mock and differ only by wire
 * protocol. Native search uses the conversation's own provider, so the
 * Anthropic case must run through an Anthropic provider to cover
 * `web_search_20250305`, `max_uses`, and `pause_turn` continuation. */
function makeProvider(runId: string, family: "openai_responses" | "anthropic"): {
  provider: ApiProvider;
  model: ModelProfile;
} {
  const suffix = family === "anthropic" ? "anthropic" : "openai";
  const model: ModelProfile = {
    id: `web-search-e2e-model-${suffix}-${runId}`,
    contextWindow: 128_000,
    maxOutputTokens: 8_192,
    name: "",
    group: "",
    capabilities: ["image_recognition"],
    reasoningContent: family === "anthropic" ? "plaintext" : "encrypted",
    promptCache: true,
  };
  return {
    model,
    provider: {
      id: `web-search-e2e-provider-${suffix}-${runId}`,
      name: `Mework Web Search E2E Mock (${suffix})`,
      enabled: true,
      familySettings: {},
      endpointBaseUrls: {},
      notes: "",
      family,
      baseUrl: protocolBaseUrl,
      models: [model],
      activeModelId: model.id,
    },
  };
}

async function saveAndFlush(documentValue: AppDocument): Promise<void> {
  await saveDocument(documentValue);
  await flushDocumentSaves();
}

function modelRequest(
  documentValue: AppDocument,
  conversationId: string,
  provider: ApiProvider,
  model: ModelProfile,
): ModelRunRequest {
  const workspace = documentValue.workspaces.find((candidate) =>
    candidate.conversations.some(
      (conversation) => conversation.id === conversationId,
    ),
  );
  assert(workspace, `E2E 对话 ${conversationId} 没有所属工作区`);
  const conversation = findConversation(documentValue, conversationId);
  return {
    provider,
    model,
    reasoningEffort: "disabled",
    conversationId,
    workspacePath: workspace.path,
    systemPrompt: conversation.settings.systemPrompt,
    enabledTools: [...conversation.settings.enabledTools],
    contexts: clone(conversation.contexts),
    tools: clone(documentValue.tools),
  };
}

async function runModelDirect(
  request: ModelRunRequest,
  requestId: string,
): Promise<{
  response: ModelRunResponse;
  events: Array<ModelStreamEvent | DebugRequestEvent>;
}> {
  const events: Array<ModelStreamEvent | DebugRequestEvent> = [];
  const channel = new Channel<ModelStreamEvent | DebugRequestEvent>();
  channel.onmessage = (event) => events.push(event);
  const response = await invoke<ModelRunResponse>("run_model", {
    request,
    requestId,
    onEvent: channel,
  });
  assertNoSensitiveValues(events, `${requestId} stream events`);
  return { response, events };
}

function toolContext(contexts: ContextItem[], toolName: string): ToolContext {
  const context = contexts.find(
    (candidate): candidate is ToolContext =>
      candidate.kind === "tool" && candidate.toolName === toolName,
  );
  assert(context, `模型回合没有产生 ${toolName} 工具记录`);
  return context;
}

function webSearchToolContexts(
  contexts: ContextItem[],
  expected: number,
  label: string,
): ToolContext[] {
  const found = contexts.filter(
    (candidate): candidate is ToolContext =>
      candidate.kind === "tool" && candidate.toolName === "web_search",
  );
  assert(
    found.length === expected,
    `${label} web_search 调用数量不是 ${expected}`,
  );
  assert(
    new Set(found.map((context) => context.id)).size === expected,
    `${label} 多个执行分支共用了同一个 web_search context`,
  );
  return found;
}

/** The mock plants this inside the upstream's search results. It must never
 * reach the parent conversation: only the search call's own findings text does. */
const UNTRUSTED_MARKER = "MEWORK_UNTRUSTED_PROMPT_INJECTION";

/** `web_search` is a concurrent asynchronous tool. Its output is the findings
 * envelope itself: no derived confirmation, `web_search:<id>` task address, or
 * `task_wait` delivery is allowed.
 *
 * `findings` is opaque text, not a parsed structure: the host sanitizes it and
 * passes it through. Asserting a JSON shape here would re-impose the contract
 * that was deliberately removed. What must hold is that raw page body never
 * crossed the boundary. */
function webSearchFindings(context: ToolContext, label: string): string {
  assert(
    context.result.success,
    `${label} 工具执行失败：${context.result.output}`,
  );
  const output = context.result.output;
  assert(
    !/web_search:search-\d+/.test(output),
    `${label} 仍然铸了任务地址：检索不再是任务`,
  );
  assert(
    !context.subagent,
    `${label} 携带了子代理记录：检索不是子代理`,
  );
  assert(
    /"untrustedWebContent":\s*true/.test(output) &&
      /"findings":/.test(output) &&
      output.includes("reported data"),
    `${label} 交付信封缺少不可信网页内容标记`,
  );
  // Native backends must deliver findings text rather than a catalog-provider
  // `results` array.
  assert(
    !/"results":/.test(output),
    `${label} 返回了目录提供商的结果数组：原生后端不该走到那条腿`,
  );
  assert(
    !output.includes(UNTRUSTED_MARKER),
    `${label} 不可信上游文本越过搜索边界进入主上下文`,
  );
  return output;
}

function assistantContains(
  response: ModelRunResponse,
  marker: string,
): boolean {
  return response.contexts.some(
    (context) =>
      context.kind === "assistant" && context.content.includes(marker),
  );
}

function stableJson(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, item]) => item !== undefined)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
    return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${stableJson(item)}`).join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

async function conversationContextDigest(
  conversation: Conversation,
): Promise<string> {
  const bytes = new TextEncoder().encode(stableJson(conversation.contexts));
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function responseTerminalError(response: ModelRunResponse): string | null {
  if (response.stopReason !== "error" && !response.error) return null;
  return response.error?.message ?? "模型续轮以 error 结束";
}

async function appendUserAndRun(
  documentValue: AppDocument,
  conversationId: string,
  provider: ApiProvider,
  model: ModelProfile,
  content: string,
  suffix: string,
): Promise<{
  response: ModelRunResponse;
  events: Array<ModelStreamEvent | DebugRequestEvent>;
}> {
  const conversation = findConversation(documentValue, conversationId);
  const user: ContextItem = {
    id: createId(`web-search-e2e-user-${suffix}`),
    kind: "user",
    content,
    createdAt: new Date().toISOString(),
  };
  conversation.contexts.push(user);
  conversation.updatedAt = new Date().toISOString();
  await saveAndFlush(documentValue);
  const run = await runModelDirect(
    modelRequest(documentValue, conversationId, provider, model),
    createId(`web-search-e2e-run-${suffix}`),
  );
  conversation.contexts.push(...clone(run.response.contexts));
  conversation.updatedAt = new Date().toISOString();
  await saveAndFlush(documentValue);
  return run;
}

function blockedDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function validBackendInstanceId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{24,128}$/.test(value);
}

async function waitForRestartedBackend(
  previousInstanceId: string,
): Promise<string> {
  const deadline = Date.now() + 120_000;
  let lastError = "尚未观察到新实例";
  while (Date.now() < deadline) {
    await new Promise((resolve) => window.setTimeout(resolve, 250));
    try {
      const instanceId = await invoke<string>("browser_e2e_instance_id");
      if (!validBackendInstanceId(instanceId)) {
        lastError = "后端返回了无效实例 ID";
        continue;
      }
      if (instanceId !== previousInstanceId) return instanceId;
      lastError = "仍连接旧实例";
    } catch (error) {
      // A refused/closed WebSocket is expected while the wrapper replaces Rust. BrowserDevClient
      // reconnects on the next invoke, so retain only a non-sensitive diagnostic.
      lastError = blockedDetail(error);
    }
  }
  throw new Error(`等待 browser-dev Rust 后端重启超时：${lastError}`);
}

async function configureKeys(providers: ApiProvider[]): Promise<void> {
  // Native backend requests use each conversation's provider credentials. Do
  // not touch catalog-provider credentials because fixed catalog IDs could
  // overwrite user configuration.
  for (const provider of providers) {
    const status = await saveApiKey(provider, MODEL_API_KEY);
    assert(status.configured, `本地 protocol mock 假 Key 未保存（${provider.id}）`);
  }
  record(
    "mock-provider-config",
    "PASS",
    "两个 mock 提供商的假 Key 均已写入 OS 凭据库，未进入文档；搜索提供商目录未被触碰",
  );
}

async function cleanup(
  state: Pick<
    ReloadState,
    | "provider"
    | "anthropicProvider"
    | "conversationIds"
    | "originalWebSearch"
    | "originalActiveProviderId"
  >,
): Promise<void> {
  const browserCleanupErrors: string[] = [];
  for (const conversationId of state.conversationIds) {
    let status: BrowserStatus;
    try {
      // browser_status is deliberately non-creating. The previous cleanup path called
      // clear_data first, which created a never-opened session, permanently marked it as User
      // controlled, and then failed to close it because browser_action("close") had no lifecycle
      // epoch. A page reload consequently failed its first Agent navigation ownership check.
      status = await invoke<BrowserStatus>("browser_status", {
        sessionId: conversationId,
      });
    } catch (error) {
      browserCleanupErrors.push(`${conversationId} status: ${blockedDetail(error)}`);
      continue;
    }
    if (!status.hasPage) continue;

    try {
      await invoke("browser_action", {
        sessionId: conversationId,
        action: "clear_data",
        value: null,
      });
    } catch (error) {
      browserCleanupErrors.push(`${conversationId} clear_data: ${blockedDetail(error)}`);
    }
    try {
      const disposition = await invoke<BrowserCloseDisposition>("browser_close", {
        sessionId: conversationId,
        lifecycleEpoch: issueBrowserLifecycleIntentEpoch(),
      });
      if (!disposition.intentAccepted || !disposition.cleanupComplete) {
        browserCleanupErrors.push(
          `${conversationId} close: ${disposition.message ?? disposition.errorCode ?? disposition.status}`,
        );
      }
    } catch (error) {
      browserCleanupErrors.push(`${conversationId} close: ${blockedDetail(error)}`);
    }
  }

  // This host-only cleanup derives the exact provider id from the runner-owned run id.
  // It deliberately does not depend on the document still containing the record: persistence
  // corruption is precisely when ordinary document-gated key deletion would otherwise leak it.
  // The search side stores no E2E credentials at all — engine ids are the fixed catalog, so a
  // fake engine key would overwrite a real one — hence there is nothing search-side to clean.
  const keyCleanup = object(
    await invoke("browser_e2e_cleanup_web_search_keys"),
    "browser_e2e_cleanup_web_search_keys",
  );
  const cleanedProviderIds = Array.isArray(keyCleanup.providerIds)
    ? keyCleanup.providerIds
    : [];
  assert(
    cleanedProviderIds.length === 2 &&
      cleanedProviderIds.includes(state.provider.id) &&
      cleanedProviderIds.includes(state.anthropicProvider.id),
    "宿主清理了非预期的模型 provider ID",
  );
  assert(keyCleanup.providerConfigured === false, "模型假 Key 清理后仍存在");

  const current = await loadDocument();
  const runProviderIds = new Set([state.provider.id, state.anthropicProvider.id]);
  current.globalSettings.apiProviders =
    current.globalSettings.apiProviders.filter(
      (provider) => !runProviderIds.has(provider.id),
    );
  current.globalSettings.activeProviderId = state.originalActiveProviderId;
  current.globalSettings.webSearch = clone(state.originalWebSearch);
  for (const workspace of current.workspaces) {
    workspace.conversations = workspace.conversations.filter(
      (conversation) => !state.conversationIds.includes(conversation.id),
    );
  }
  await saveAndFlush(current);

  const cleaned = await loadDocument();
  assert(
    !cleaned.globalSettings.apiProviders.some((provider) =>
      runProviderIds.has(provider.id),
    ),
    "E2E provider 仍留在文档",
  );
  assert(
    !cleaned.workspaces.some((workspace) =>
      workspace.conversations.some((conversation) =>
        state.conversationIds.includes(conversation.id),
      ),
    ),
    "E2E 对话仍留在文档",
  );
  assertNoSensitiveValues(cleaned, "cleanup 后文档");
  assert(
    browserCleanupErrors.length === 0,
    `普通浏览器清理失败：${browserCleanupErrors.join("；")}`,
  );
}

async function postReport(payload: ReportPayload): Promise<void> {
  if (!reportUrl) return;
  assert(/^[0-9a-f]{64}$/.test(reportToken), "runner report token 无效");
  const response = await fetch(reportUrl, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-mework-e2e-report-token": reportToken,
    },
    body: JSON.stringify(payload),
  });
  if (!response.ok)
    throw new Error(`runner report endpoint 返回 ${response.status}`);
}

function finalStatus(): "passed" | "failed" | "blocked" {
  if (checks.some((check) => check.state === "FAIL")) return "failed";
  if (checks.some((check) => check.state === "BLOCKED")) return "blocked";
  return "passed";
}

async function finish(failureMessage?: string): Promise<void> {
  const status = failureMessage ? "failed" : finalStatus();
  if (failureMessage) {
    failure.hidden = false;
    failure.textContent = failureMessage;
  }
  summary.textContent =
    status === "passed"
      ? `PASS · ${checks.filter((check) => check.state === "PASS").length} 项真实验收通过`
      : status === "blocked"
        ? `BLOCKED · ${checks.filter((check) => check.state === "PASS").length} 项通过，${checks.filter((check) => check.state === "BLOCKED").length} 项待宿主接线`
        : "FAIL · 联网搜索 E2E 未通过";
  summary.className =
    status === "passed" ? "pass" : status === "blocked" ? "blocked" : "fail";
  document.title = `${status.toUpperCase()} · Mework Web Search E2E`;
  await postReport({
    status,
    checks,
    ...(failureMessage ? { failure: failureMessage } : {}),
  });
}

async function initialRun(): Promise<void> {
  assert(hasBackendRuntime(), "请通过 npm run test:web-search-e2e 启动此页面");
  assert(
    isLoopbackProtocolBase(protocolBaseUrl),
    "protocol mock Base URL 必须是固定回环 /v1 地址",
  );
  assert(
    isLoopbackHttpOrigin(`${browserOrigin}/`),
    "browser fixture origin 必须是固定回环 HTTP origin",
  );
  assert(
    /^[0-9a-f]{24}$/.test(configuredRunId),
    "runner-owned E2E run ID 无效",
  );

  const original = await loadDocument();
  const runId = configuredRunId;
  const { provider, model } = makeProvider(runId, "openai_responses");
  const { provider: anthropicProvider, model: anthropicModel } = makeProvider(
    runId,
    "anthropic",
  );

  // Exercise native search through Anthropic Messages with `pause_turn`, OpenAI
  // Responses, and a blocked upstream-search outcome. Each request reaches the
  // protocol mock through the production credential, request-construction, and
  // stream-parsing paths.
  //
  // Catalog-provider search makes no model request and is covered by the Rust
  // HTTP-fixture tests in `api.rs`.
  const modes = [
    {
      wire: "anthropic" as const,
      suffix: "anthropic",
      label: "Anthropic 原生服务端搜索",
      trigger: "MEWORK_WEB_SEARCH_E2E_ANTHROPIC",
      session: `web-anthropic-${runId}`,
      marker: "[WEB_ANTHROPIC_E2E_OK]",
      findings: "MEWORK_FINDINGS_ANTHROPIC",
      maxSearches: 3,
      check: "web-search-anthropic-native-tool",
      detail:
        "Anthropic 线：findings 就是这次调用自己的结果，代发请求只带原生 web_search_20250305（含 max_uses），pause_turn 续接后照常回到同一条工具记录",
    },
    {
      // Native search uses the conversation's own provider and model, not
      // catalog-provider configuration.
      wire: "openai_responses" as const,
      suffix: "openai",
      label: "OpenAI 原生服务端搜索",
      trigger: "MEWORK_WEB_SEARCH_E2E_OPENAI",
      session: `web-openai-${runId}`,
      marker: "[WEB_OPENAI_E2E_OK]",
      findings: "MEWORK_FINDINGS_OPENAI",
      maxSearches: 0,
      check: "web-search-native-backend",
      detail:
        "OpenAI 线：findings 就地返回，代发请求打到对话自己的 provider，只带不带版本的 web_search 且不伪造 max_uses",
    },
    {
      wire: "anthropic" as const,
      suffix: "error",
      label: "上游搜索受阻",
      trigger: "MEWORK_WEB_SEARCH_E2E_SEARCH_ERROR",
      session: `web-error-${runId}`,
      marker: "[WEB_SEARCH_ERROR_E2E_OK]",
      findings: "MEWORK_FINDINGS_SEARCH_ERROR",
      maxSearches: 1,
      check: "web-search-result-error-is-an-outcome",
      detail:
        "上游返回 max_uses_exceeded 时这次检索仍正常收场，说明阻塞的 findings 是一种结局而不是工具故障",
    },
  ];
  const conversationIds = modes.map(
    (mode) => `web-search-e2e-${mode.suffix}-${runId}`,
  );
  const turnConversationIds = [...conversationIds];
  const stateBase = {
    provider,
    anthropicProvider,
    conversationIds,
    turnConversationIds,
    originalWebSearch: clone(original.globalSettings.webSearch),
    originalActiveProviderId: original.globalSettings.activeProviderId,
  };

  try {
    assert(
      original.tools.some((tool) => tool.name === "web_search") &&
        original.tools.some((tool) => tool.name === "web_fetch") &&
        !original.tools.some((tool) => tool.name === "web_query"),
      "主代理工具目录必须公开 web_search 与 web_fetch 两个入口，且不含退役的 web_query",
    );

    // This run must not access catalog-provider credentials because its native
    // search modes use fixed catalog IDs that could overwrite user keys.
    record(
      "search-provider-credential-safety",
      "PASS",
      "本次运行不写、不改、不读任何搜索提供商目录条目的凭据",
    );

    const working = clone(original);
    working.globalSettings.apiProviders.push(provider, anthropicProvider);
    working.globalSettings.activeProviderId = provider.id;
    const temporary = temporaryWorkspace(working);
    temporary.conversations.push(
      ...modes.map((mode, index) =>
        makeConversation(
          conversationIds[index],
          `Web search E2E / ${mode.suffix}`,
          { kind: "native" },
          mode.maxSearches,
        ),
      ),
    );
    await saveAndFlush(working);
    await configureKeys([provider, anthropicProvider]);
    assertNoSensitiveValues(await loadDocument(), "配置后文档");
    record(
      "secret-document-boundary",
      "PASS",
      "两个 mock 提供商的假 Key 都只进入 OS 凭据库，未进入持久化 AppDocument",
    );

    for (const [index, mode] of modes.entries()) {
      const conversationId = conversationIds[index];
      // Native search uses the conversation's provider, so changing the wire
      // protocol also changes the provider.
      const run = await appendUserAndRun(
        working,
        conversationId,
        mode.wire === "anthropic" ? anthropicProvider : provider,
        mode.wire === "anthropic" ? anthropicModel : model,
        [
          mode.trigger,
          `MEWORK_PROTOCOL_E2E_SESSION=${mode.session}`,
          "请调用 web_search 完成这次检索；只报告发现，不要把搜索结果摘要当成已证实的事实。",
        ].join("\n"),
        mode.suffix,
      );
      const terminalError = responseTerminalError(run.response);
      assert(!terminalError, `${mode.label} 回合以错误结束：${terminalError}`);
      const search = toolContext(run.response.contexts, "web_search");
      const envelope = webSearchFindings(search, `${mode.label} web_search`);
      assert(
        envelope.includes(mode.findings),
        `${mode.label} 结果缺少本模式专属 findings`,
      );
      // Native search must not produce `task_wait` records.
      assert(
        !run.response.contexts.some(
          (context) => context.kind === "tool" && context.toolName === "task_wait",
        ),
        `${mode.label} 仍然经 task_wait 交付：检索不再是任务`,
      );
      assert(
        assistantContains(run.response, mode.marker),
        `${mode.label} 未产生终局标记 ${mode.marker}`,
      );
      assertNoSensitiveValues(run.response.contexts, `${mode.label} 回合上下文`);
      record(mode.check, "PASS", mode.detail);
    }

    const persisted = await loadDocument();
    for (const conversationId of turnConversationIds) {
      const conversation = findConversation(persisted, conversationId);
      assert(
        conversation.contexts.length > 1,
        `${conversationId} 未持久化模型回合`,
      );
    }
    assertNoSensitiveValues(persisted, "flush 后文档");
    record(
      "flush-before-reload",
      "PASS",
      "三种模式的检索回合均已 flush，文档仍不含任何假 Key/Cookie 值",
    );

    const reloadState: ReloadState = {
      version: 9,
      runId,
      ...stateBase,
      expectedContextIds: Object.fromEntries(
        turnConversationIds.map((conversationId) => [
          conversationId,
          findConversation(persisted, conversationId).contexts.map(
            (context) => context.id,
          ),
        ]),
      ),
      expectedContextDigests: Object.fromEntries(
        await Promise.all(
          turnConversationIds.map(async (conversationId) => [
            conversationId,
            await conversationContextDigest(
              findConversation(persisted, conversationId),
            ),
          ]),
        ),
      ),
      checks: clone(checks),
    };
    sessionStorage.setItem(RELOAD_STATE_KEY, JSON.stringify(reloadState));
    summary.textContent = "已 flush；正在重载 E2E 页面验证真实持久化恢复…";
    window.location.reload();
  } catch (error) {
    try {
      await cleanup(stateBase);
      record(
        "exact-cleanup-after-failure",
        "PASS",
        "失败路径删除了精确假 Key、浏览器数据与 E2E 对话",
      );
    } catch (cleanupError) {
      record(
        "exact-cleanup-after-failure",
        "FAIL",
        blockedDetail(cleanupError),
      );
    }
    throw error;
  }
}

async function assertPersistedE2eState(
  documentValue: AppDocument,
  state: ReloadState,
  label: string,
): Promise<void> {
  for (const conversationId of state.turnConversationIds) {
    const conversation = findConversation(documentValue, conversationId);
    const expected = state.expectedContextIds[conversationId] ?? [];
    assert(
      JSON.stringify(conversation.contexts.map((context) => context.id)) ===
        JSON.stringify(expected),
      `${label}: ${conversationId} 的上下文 ID/顺序发生变化`,
    );
    assert(
      (await conversationContextDigest(conversation)) ===
        state.expectedContextDigests[conversationId],
      `${label}: ${conversationId} 的完整上下文、执行子记录或工具证据发生变化`,
    );
    const webTools = webSearchToolContexts(
      conversation.contexts,
      1,
      `${label}: ${conversationId}`,
    );
    for (const [index, webTool] of webTools.entries()) {
      webSearchFindings(
        webTool,
        `${label}: ${conversationId} 调用 ${index + 1}`,
      );
    }
    // Detect a regression that routes delivery back through the task surface.
    assert(
      !conversation.contexts.some(
        (candidate) => candidate.kind === "tool" && candidate.toolName === "task_wait",
      ),
      `${label}: ${conversationId} 落盘里出现了 task_wait 记录：检索不再是任务`,
    );
  }
  assertNoSensitiveValues(documentValue, label);
}

async function reloadRun(state: ReloadState): Promise<void> {
  assert(state.runId === configuredRunId, "reload state 不属于当前 runner");
  sessionStorage.removeItem(RELOAD_STATE_KEY);
  assert(state.version === 9, "未知 reload state 版本");
  checks.push(...state.checks);
  for (const check of state.checks) renderCheck(check);
  try {
    const restored = await loadDocument();
    await assertPersistedE2eState(restored, state, "页面重载后文档");
    record(
      "page-reload-persistence",
      "PASS",
      "页面重载后恢复相同 context ID/完整内容、通道边界、联网搜索证据与可读取 PNG 附件",
    );

    const previousInstanceId = await invoke<string>("browser_e2e_instance_id");
    assert(
      validBackendInstanceId(previousInstanceId),
      "browser-dev 初始实例 ID 无效",
    );
    let restartAcknowledged = false;
    try {
      const restart = object(
        await invoke<RestartAcceptance>("browser_e2e_restart_backend", {
          instanceId: previousInstanceId,
        }),
        "browser_e2e_restart_backend",
      );
      assert(
        restart.accepted === true && restart.instanceId === previousInstanceId,
        "browser-dev 没有确认精确旧实例的重启请求",
      );
      restartAcknowledged = true;
    } catch {
      // The host may close the WebSocket after accepting the request but before the queued result
      // reaches this renderer. A different authenticated instance id is authoritative evidence:
      // the wrapper restarts only after both request and post-cleanup commit markers from that same
      // Rust child. Policy errors or ordinary crashes cannot produce such a replacement instance.
    }
    const restartedInstanceId =
      await waitForRestartedBackend(previousInstanceId);
    assert(
      restartedInstanceId !== previousInstanceId,
      "browser-dev 重启后实例 ID 没有变化",
    );
    const restarted = await loadDocument();
    await assertPersistedE2eState(restarted, state, "Rust 进程重启后文档");
    record(
      "process-restart-persistence",
      "PASS",
      `Rust 后端已优雅退出并由同一 wrapper 以新实例重启（bridge ACK ${
        restartAcknowledged ? "已送达" : "随旧连接关闭"
      }）；隔离文档、通道边界及 PNG 附件均恢复`,
    );
    await cleanup(state);
    record(
      "exact-cleanup",
      "PASS",
      "清除三个普通浏览器 profile，删除精确 provider 假 Key，并移除 E2E 对话/设置",
    );
    await finish();
  } catch (error) {
    try {
      await cleanup(state);
      record(
        "exact-cleanup-after-reload-failure",
        "PASS",
        "重载失败路径仍完成精确清理",
      );
    } catch (cleanupError) {
      record(
        "exact-cleanup-after-reload-failure",
        "FAIL",
        blockedDetail(cleanupError),
      );
    }
    throw error;
  }
}

async function main(): Promise<void> {
  for (const key of LEGACY_RELOAD_STATE_KEYS) sessionStorage.removeItem(key);
  const rawState = sessionStorage.getItem(RELOAD_STATE_KEY);
  if (rawState) {
    const state = JSON.parse(rawState) as ReloadState;
    if (state.version === 9 && state.runId === configuredRunId) {
      await reloadRun(state);
      return;
    }
    sessionStorage.removeItem(RELOAD_STATE_KEY);
  }
  await initialRun();
}

main().catch(async (error) => {
  const message =
    error instanceof Error ? (error.stack ?? error.message) : String(error);
  record(
    "fatal",
    "FAIL",
    error instanceof Error ? error.message : String(error),
  );
  try {
    await finish(message);
  } catch (reportError) {
    failure.hidden = false;
    failure.textContent = `${message}\n\n无法报告给 runner：${blockedDetail(reportError)}`;
  }
});
