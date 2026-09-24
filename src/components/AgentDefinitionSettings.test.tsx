import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type {
  AgentDefinition,
  ApiProvider,
  ContextItem,
  ConversationPreset,
  ConversationTemplateSummary,
  ToolDescriptor,
  WebSearchAssets
} from "../types";
import { emptyConversationPresetSettings } from "../lib/conversationPresets";
import {
  DEFAULT_SEARCH_COMPRESSION_CUTOFF,
  DEFAULT_SEARCH_MAX_RESULTS
} from "../lib/searchProviders";
import { AgentDefinitionSettings } from "./AgentDefinitionSettings";

const exactProviderId = "provider:/精确";
const exactModelId = "kimi/vision:v4-模型";

const providers: ApiProvider[] = [{
  id: exactProviderId,
  name: "Exact Provider",
  enabled: true,
  endpointBaseUrls: {},
  familySettings: {},
  notes: "",
  family: "openai_chat",
  baseUrl: "https://example.invalid/v1",
  activeModelId: exactModelId,
  models: [{
    id: exactModelId,
    name: "",
    group: "",
    capabilities: ["image_recognition"],
    reasoningContent: "plaintext",
    promptCache: true
  }, {
    id: "second-model",
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: "plaintext",
    promptCache: true
  }]
}, {
  id: "disabled-provider",
  name: "Disabled Provider",
  enabled: false,
  endpointBaseUrls: {},
  familySettings: {},
  notes: "",
  family: "openai_chat",
  baseUrl: "https://disabled.invalid/v1",
  activeModelId: "hidden-model",
  models: [{
    id: "hidden-model",
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: "plaintext",
    promptCache: true
  }]
}];

const tools: ToolDescriptor[] = [{
  name: "read_file",
  label: "读取文件",
  description: "",
  category: "filesystem",
  dangerous: false,
  parameters: []
}, {
  name: "run_command",
  label: "运行命令",
  description: "",
  category: "shell",
  dangerous: true,
  parameters: []
}, {
  // The editor must exclude every orchestration tool, not just `workflow`.
  // These names ensure the test detects category-based filtering.
  name: "agent_spawn",
  label: "子代理工具（不应出现）",
  description: "",
  category: "orchestration",
  dangerous: false,
  parameters: []
}, {
  name: "send_message",
  label: "发送消息工具（不应出现）",
  description: "",
  category: "orchestration",
  dangerous: false,
  parameters: []
}, {
  name: "followup_task",
  label: "追加任务工具（不应出现）",
  description: "",
  category: "orchestration",
  dangerous: false,
  parameters: []
}, {
  name: "todo",
  label: "待办事项工具（不应出现）",
  description: "",
  category: "orchestration",
  dangerous: false,
  parameters: []
}, {
  name: "workflow",
  label: "工作流工具（不应出现）",
  description: "",
  category: "orchestration",
  dangerous: false,
  parameters: []
}];

// Keep the parent enabled so dependent child rows are rendered for the test.
const conversationEnabledTools = ["read_file", "agent_spawn", "send_message", "todo", "workflow"];

/* A role's template body never travels with the role: the host stores it and
   hands back the id it landed under. These stand in for that store. */
const readTemplate = vi.fn(async (_templateId: string): Promise<ContextItem[]> => []);
const writeTemplate = vi.fn(async (templateId: string): Promise<string> => templateId || "template_minted");

// A catalog row the role editor can actually offer: the picker lists only
// providers that are switched on.
const webSearchAssets: WebSearchAssets = {
  providers: [{
    kind: "tavily",
    enabled: true,
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  }, {
    // Tavily searches but cannot fetch, so the fetch picker needs a row of its
    // own to offer — the two legs list different catalogues.
    kind: "jina",
    enabled: true,
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  }]
};

function userDefinition(overrides: Partial<AgentDefinition> = {}): AgentDefinition {
  return {
    enabled: true,
    deleted: false,
    name: "reviewer",
    description: "",
    source: "user",
    sourceKey: "",
    revision: 4,
    memoryEpoch: 3,
    modelSelection: { kind: "inherit" },
    memory: "none",
    effort: null,
    tools: null,
    disallowedTools: [],
    searchProvider: null,
    fetchProvider: null,
    maxResults: DEFAULT_SEARCH_MAX_RESULTS,
    compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
    domainFilter: null,
    includeDomains: [],
    excludeDomains: [],
    templateId: null,
    ...overrides
  };
}

/* The editor keeps an in-progress role per list, and `listId` already carries
   the conversation in the application. So each case gets a list of its own and
   cannot inherit the previous one's half-written role; the two cases that are
   ABOUT that cache pass an explicit id and reuse it on purpose. */
let listSeq = 0;

function nextListId(): string {
  listSeq += 1;
  return `roles-${listSeq}`;
}

function renderSettings(
  definitions: readonly AgentDefinition[] = [],
  onChange = vi.fn(),
  presets: readonly ConversationPreset[] = [],
  listId = nextListId(),
  templates: ConversationTemplateSummary[] = []
) {
  render(
    <AgentDefinitionSettings
      definitions={definitions}
      listId={listId}
      providers={providers}
      tools={tools}
      conversationEnabledTools={conversationEnabledTools}
      webSearchAssets={webSearchAssets}
      templates={templates}
      presets={presets}
      onReadTemplate={readTemplate}
      onWriteTemplate={writeTemplate}
      onChange={onChange}
    />
  );
  return onChange;
}

/** Opens one of the role window's pages from its rail. */
async function openPage(
  user: ReturnType<typeof userEvent.setup>,
  dialog: HTMLElement,
  page: RegExp
) {
  const rail = within(dialog).getByRole("navigation", { name: "角色设置分类" });
  await user.click(within(rail).getByRole("button", { name: page }));
}

function templateSummary(id: string, messageCount: number): ConversationTemplateSummary {
  return { id, name: "", messageCount, createdAt: "", updatedAt: "" };
}

function userMessage(id: string, content: string): ContextItem {
  return { id, kind: "user", content, createdAt: "2026-01-01T00:00:00.000Z" };
}

afterEach(() => {
  cleanup();
  readTemplate.mockReset();
  readTemplate.mockImplementation(async () => []);
  writeTemplate.mockClear();
  configureI18n("zh-CN");
});

describe("AgentDefinitionSettings", () => {
  it.each([
    ["zh-CN" as const, "新建角色", "角色名称", "保存角色", "角色名称不能为空。", "请先修正标记的字段。"],
    ["en-US" as const, "New role", "Role name", "Save role", "A role name is required.", "Fix the marked fields before saving."]
  ])("the plus row opens a create dialog that asks for the name only on save (%s)", async (
    language,
    createLabel,
    nameLabel,
    saveLabel,
    requiredHint,
    banner
  ) => {
    configureI18n(language);
    const user = userEvent.setup();
    const onChange = renderSettings();

    const create = screen.getByRole("button", { name: createLabel });
    expect(create).toBeInTheDocument();
    await user.click(create);

    const dialog = screen.getByRole("dialog", { name: createLabel });
    expect(within(dialog).getByRole("textbox", { name: nameLabel })).toHaveValue("");
    // A create dialog opens on a blank name, and complaining about a name the
    // user has not had a chance to type yet reads as a scold — so the complaint
    // is withheld until Save asks the question.
    expect(within(dialog).queryByText(requiredHint)).not.toBeInTheDocument();
    const save = within(dialog).getByRole("button", { name: saveLabel });
    expect(save).toBeEnabled();

    await user.click(save);

    // Save is what asks: it is the moment the name has to be right, so it is
    // also the moment the field is allowed to say so.
    expect(within(dialog).getByText(requiredHint)).toBeInTheDocument();
    expect(within(dialog).getByText(banner)).toBeInTheDocument();
    // The save is refused rather than applied, so the dialog stays put for the
    // user to fix the name in it.
    expect(screen.getByRole("dialog", { name: createLabel })).toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("lays the role out as a preset's window is: three pages down a rail, the save under it", async () => {
    const user = userEvent.setup();
    renderSettings([userDefinition()]);

    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    // Named after the role, the way a preset's window is named after the preset.
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    const rail = within(dialog).getByRole("navigation", { name: "角色设置分类" });
    expect(within(rail).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["角色设置", "工具", "对话模板0", "保存角色"]);
    // It opens on the role's own settings, and only those: the tool surface is
    // a page of its own.
    expect(within(dialog).getByRole("textbox", { name: "角色名称" })).toBeInTheDocument();
    expect(within(dialog).getByRole("combobox", { name: "执行模型" })).toBeInTheDocument();
    expect(within(dialog).queryByText("启用工具")).toBeNull();

    await openPage(user, dialog, /^工具/);
    expect(within(dialog).getByText("启用工具")).toBeInTheDocument();
    expect(within(dialog).queryByRole("textbox", { name: "角色名称" })).toBeNull();
    // The conversation's own features page, minus what a role has no field for.
    expect(within(dialog).queryByRole("switch", { name: /联网搜索/ })).toBeNull();
    expect(within(dialog).queryByRole("switch", { name: /记忆/ })).toBeNull();
    expect(within(dialog).queryByRole("switch", { name: /应用数据目录/ })).toBeNull();
    expect(within(dialog).queryByRole("combobox", { name: "工具描述" })).toBeNull();
  });

  it("edits the role's template on its own page and slides the presets in under the rail", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings([userDefinition()], vi.fn(), [{
      id: "conversation_default",
      name: "默认",
      description: "",
      templateId: "template_preset",
      settings: emptyConversationPresetSettings()
    }, {
      id: "conversation_blank",
      name: "空白",
      description: "",
      templateId: "",
      settings: emptyConversationPresetSettings()
    }], nextListId(), [templateSummary("template_preset", 2)]);
    readTemplate.mockImplementation(async (templateId: string) => (
      templateId === "template_preset"
        ? [userMessage("ctx_a", "预设的第一句"), userMessage("ctx_b", "预设的第二句")]
        : []
    ));

    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    // The presets belong to the template page, so they are not on the rail
    // until that page is open.
    expect(within(dialog).queryByText("从预设覆盖")).toBeNull();

    await openPage(user, dialog, /^对话模板/);
    expect(within(dialog).getByText("从预设覆盖")).toBeInTheDocument();
    const presets = within(dialog).getByRole("navigation", { name: "角色设置分类" });
    // Edits are written as they land, as on a preset's page: no save of its own.
    expect(within(dialog).queryByRole("button", { name: /保存模板/ })).toBeNull();
    // A preset with no template has nothing to copy.
    expect(within(presets).getByRole("button", { name: "用预设 空白 的对话模板覆盖" })).toBeDisabled();

    // The role's template is empty, so there is nothing to lose and nothing to ask.
    await user.click(within(presets).getByRole("button", { name: "用预设 默认 的对话模板覆盖" }));
    expect(screen.queryByRole("dialog", { name: "覆盖对话模板？" })).toBeNull();
    expect(readTemplate).toHaveBeenCalledWith("template_preset");
    expect(writeTemplate).toHaveBeenCalledWith("", [
      userMessage("ctx_a", "预设的第一句"),
      userMessage("ctx_b", "预设的第二句")
    ]);
    expect(await within(dialog).findByText("预设的第一句")).toBeInTheDocument();

    // The minted id reaches the role through its draft, on Save.
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));
    expect(onChange.mock.calls.at(-1)![0][0].templateId).toBe("template_minted");
  });

  it("asks before a preset's template replaces one the role already has", async () => {
    const user = userEvent.setup();
    renderSettings([userDefinition({ templateId: "template_role" })], vi.fn(), [{
      id: "conversation_default",
      name: "默认",
      description: "",
      templateId: "template_preset",
      settings: emptyConversationPresetSettings()
    }], nextListId(), [templateSummary("template_role", 1), templateSummary("template_preset", 1)]);
    readTemplate.mockImplementation(async (templateId: string) => (
      templateId === "template_role"
        ? [userMessage("ctx_own", "角色自己的开场")]
        : [userMessage("ctx_preset", "预设的开场")]
    ));

    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    await openPage(user, dialog, /^对话模板/);
    expect(await within(dialog).findByText("角色自己的开场")).toBeInTheDocument();

    const row = within(dialog).getByRole("button", { name: "用预设 默认 的对话模板覆盖" });
    await user.click(row);
    // Declining leaves the role's template exactly as it was.
    await user.click(within(screen.getByRole("dialog", { name: "覆盖对话模板？" }))
      .getByRole("button", { name: "取消" }));
    expect(writeTemplate).not.toHaveBeenCalled();
    expect(within(dialog).getByText("角色自己的开场")).toBeInTheDocument();

    await user.click(row);
    const confirm = screen.getByRole("dialog", { name: "覆盖对话模板？" });
    expect(confirm).toHaveTextContent("已有 1 条消息");
    await user.click(within(confirm).getByRole("button", { name: "覆盖" }));

    expect(writeTemplate).toHaveBeenCalledWith("template_role", [userMessage("ctx_preset", "预设的开场")]);
    expect(await within(dialog).findByText("预设的开场")).toBeInTheDocument();
    expect(within(dialog).queryByText("角色自己的开场")).toBeNull();
  });

  it("does not dismiss an in-progress editor when its backdrop is clicked", async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    const backdrop = dialog.parentElement;
    expect(backdrop).toHaveClass("modal-backdrop");

    fireEvent.mouseDown(backdrop!);

    expect(screen.getByRole("dialog", { name: "新建角色" })).toBeInTheDocument();
  });

  it("creates a host-owned role from an exact enabled provider/model selection", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });

    await user.type(within(dialog).getByRole("textbox", { name: "角色名称" }), "security-reviewer");
    // The role description is appended to agent_spawn or workflow inputs.
    await user.type(
      within(dialog).getByRole("textbox", { name: "子代理描述" }),
      "对抗式审查。"
    );
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "执行模型" }),
      within(dialog).getByRole("option", { name: `Exact Provider · ${exactModelId}` })
    );

    // Every model installed on an enabled provider is offered; only the disabled
    // provider's model stays out.
    expect(within(dialog).getByRole("option", { name: /second-model/ })).toBeInTheDocument();
    expect(within(dialog).queryByRole("option", { name: /hidden-model/ })).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("textbox", { name: /系统提示词/ })).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("combobox", { name: /独立记忆/ })).not.toBeInTheDocument();
    expect(within(dialog).queryByRole("textbox", { name: /最大轮数/ })).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange).toHaveBeenCalledWith([{
      enabled: true,
      deleted: false,
      name: "security-reviewer",
      description: "对抗式审查。",
      source: "user",
      sourceKey: "",
      revision: 1,
      memoryEpoch: 1,
      modelSelection: {
        kind: "explicit",
        providerId: exactProviderId,
        modelId: exactModelId
      },
      memory: "none",
      effort: null,
      tools: null,
      disallowedTools: [],
      searchProvider: null,
      fetchProvider: null,
      // A new role opens on the shared result shaping rather than on 0: 0 is
      // the "no cap" answer, and nobody chose it here.
      maxResults: DEFAULT_SEARCH_MAX_RESULTS,
      compressionCutoff: DEFAULT_SEARCH_COMPRESSION_CUTOFF,
      domainFilter: null,
      includeDomains: [],
      excludeDomains: [],
      templateId: null
    }]);
  });

  it("shows the resolved model id and explains when a bound model is unavailable", () => {
    renderSettings([
      userDefinition({
        name: "live-role",
        modelSelection: { kind: "explicit", providerId: exactProviderId, modelId: exactModelId }
      }),
      userDefinition({
        name: "gone-role",
        modelSelection: { kind: "explicit", providerId: exactProviderId, modelId: "removed-model" }
      }),
      userDefinition({ name: "inheriting" })
    ]);

    // The row is one line, so the model the role runs on rides the row's own
    // title rather than a second line of text.
    const rowOf = (name: string) => screen
      .getByRole("button", { name: `设置角色 ${name}` })
      .closest(".catalog-row") as HTMLElement;
    const live = rowOf("live-role");
    const gone = rowOf("gone-role");
    expect(live).toHaveAttribute("title", expect.stringContaining(exactModelId));
    expect(gone).toHaveAttribute(
      "title",
      expect.stringContaining("removed-model · 模型暂时取不到，模型看不到这个角色")
    );
    expect(rowOf("inheriting"))
      .toHaveAttribute("title", expect.stringContaining("跟随对话模型"));
    // An unresolvable binding is uncallable, and the row has to say so or it
    // reads as a normal row.
    expect(within(gone).getByText("模型不可用")).toBeInTheDocument();
    expect(within(live).queryByText("模型不可用")).not.toBeInTheDocument();
  });

  it("renders a legacy unavailable role without naming the model it lost", async () => {
    // An older build discarded the dead provider/model IDs on demotion, so the
    // row has nothing to print — and must not imply the role still works.
    const user = userEvent.setup();
    renderSettings([
      userDefinition({ name: "gone-role", modelSelection: { kind: "unavailable" } })
    ]);

    const row = screen.getByRole("button", { name: "设置角色 gone-role" })
      .closest(".catalog-row") as HTMLElement;
    expect(row).toHaveAttribute(
      "title",
      expect.stringContaining("原模型已不可用，模型看不到这个角色")
    );

    await user.click(screen.getByRole("button", { name: "设置角色 gone-role" }));
    const dialog = screen.getByRole("dialog", { name: "gone-role" });
    const select = within(dialog).getByRole("combobox", { name: "执行模型" }) as HTMLSelectElement;
    // Nothing is selected: the dead binding gets no option of its own, so the
    // dropdown cannot present it as a choice — and must not fall back to
    // It must not fall back to the inherit-model option, which is a different
    // working configuration.
    expect(select.value).toBe("");
    expect(within(dialog).queryByRole("option", { name: "（原模型已不可用）" })).toBeNull();
    expect(select).toBeInvalid();
  });

  it("switching a role off advances its revision and leaves every other field alone", async () => {
    // `enabled` is capability-bearing: the host compares it, so flipping the
    // switch must move the revision forward — and must change nothing else
    // about the definition.
    const user = userEvent.setup();
    const existing = userDefinition({ enabled: true, revision: 4 });
    const onChange = renderSettings([existing]);

    await user.click(screen.getByRole("switch", { name: "角色 reviewer 已启用" }));

    expect(onChange).toHaveBeenCalledWith([{
      ...existing,
      enabled: false,
      revision: existing.revision + 1
    }]);
  });

  it("keeps a binding whose provider is only disabled, and says it comes back", async () => {
    // Disabled is not absent: retain the binding so reenabling restores it.
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderSettings(
      [userDefinition({
        name: "paused-role",
        modelSelection: { kind: "explicit", providerId: "disabled-provider", modelId: "hidden-model" }
      })],
      onChange
    );

    await user.click(screen.getByRole("button", { name: "设置角色 paused-role" }));
    const dialog = screen.getByRole("dialog", { name: "paused-role" });
    const select = within(dialog).getByRole("combobox", { name: "执行模型" }) as HTMLSelectElement;
    expect(select).toBeInvalid();
    // The provider's display name, never its raw ID: that ID is a random
    // per-installation UUID in the product and reads to the user as a hash.
    expect(within(dialog).getByText("Disabled Provider · hidden-model（不可用）")).toBeInTheDocument();
    expect(within(dialog).getByText(
      "这个模型现在取不到——提供商还没拉取模型、被停用，或者这一行已经不在了。角色暂时不能被调用，但绑定会一直留着，模型回来就自动恢复。"
    )).toBeInTheDocument();

    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "推理强度" }),
      within(dialog).getByRole("option", { name: "high" })
    );
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    const saved = onChange.mock.calls.at(-1)![0][0];
    expect(saved.modelSelection).toEqual({
      kind: "explicit",
      providerId: "disabled-provider",
      modelId: "hidden-model"
    });
  });

  it("names a binding whose provider row is gone by its model id alone", async () => {
    // With no row left there is no display name to give, and the raw providerId
    // is a random UUID — printing it would show the user a hash instead of the
    // one identifier they can still recognize.
    const user = userEvent.setup();
    renderSettings([userDefinition({
      name: "orphan-role",
      modelSelection: { kind: "explicit", providerId: "provider_a1b2c3", modelId: "gpt-5.6-sol" }
    })]);

    await user.click(screen.getByRole("button", { name: "设置角色 orphan-role" }));
    const dialog = screen.getByRole("dialog", { name: "orphan-role" });
    expect(within(dialog).getByText("gpt-5.6-sol（不可用）")).toBeInTheDocument();
    expect(within(dialog).queryByText(/provider_a1b2c3/u)).toBeNull();
  });

  it("saves an unavailable role instead of trapping every other edit behind it", async () => {
    // The role arrives in this state on its own, so blocking the save would
    // hold a rename hostage to restoring a provider the user may not control.
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderSettings(
      [userDefinition({ name: "gone-role", modelSelection: { kind: "unavailable" } })],
      onChange
    );

    await user.click(screen.getByRole("button", { name: "设置角色 gone-role" }));
    const dialog = screen.getByRole("dialog", { name: "gone-role" });
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "推理强度" }),
      within(dialog).getByRole("option", { name: "high" })
    );
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange).toHaveBeenCalled();
    const saved = onChange.mock.calls.at(-1)![0][0];
    expect(saved.modelSelection).toEqual({ kind: "unavailable" });
    expect(saved.effort).toBe("high");
  });

  it("selects tools by group and ends its heading with the way in to the docs", async () => {
    const user = userEvent.setup();
    const existing = userDefinition({ tools: ["run_command"] });
    const onChange = renderSettings([existing]);
    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    await openPage(user, dialog, /^工具/);

    /* The three bulk buttons that used to sit in this heading are gone. They
       acted on the whole catalogue at once, which is not how anyone picks a
       tool surface; each group now carries its own pair, and the heading ends
       the way the conversation's own tool picker ends — with the page that
       explains what these entries are. */
    for (const gone of ["全部启用", "全部关闭", "继承对话设置"]) {
      expect(within(dialog).queryByRole("button", { name: gone })).toBeNull();
    }
    expect(within(dialog).getByRole("link", { name: "配置说明文档" })).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "全不选Shell" }));
    expect(within(dialog).getByText("0 / 2 个已选")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "全选文件与搜索" }));
    await user.click(within(dialog).getByRole("button", { name: "全选Shell" }));
    expect(within(dialog).getByText("2 / 2 个已选")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange).toHaveBeenCalledWith([{
      ...existing,
      revision: existing.revision + 1,
      tools: ["read_file", "run_command"]
    }]);
  });

  /* The selector shows the complete catalog, including tools the conversation
   * did not enable. Saving the selection must preserve that granted capability. */
  it("grants a catalogue tool the conversation itself did not enable", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(
      within(dialog).getByRole("textbox", { name: "角色名称" }),
      "runner"
    );
    await openPage(user, dialog, /^工具/);

    expect(conversationEnabledTools).not.toContain("run_command");
    await user.click(within(dialog).getByRole("button", { name: "运行命令已关闭" }));
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    // Toggling a tool converts inherited settings to an explicit list.
    expect(onChange.mock.calls.at(-1)![0][0].tools).toEqual(["read_file", "run_command"]);
  });

  it("carries a per-role search backend, defaulting to following the conversation", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(
      within(dialog).getByRole("textbox", { name: "角色名称" }),
      "searcher"
    );
    await openPage(user, dialog, /^工具/);

    /* A menu, not a `<select>`: the same control the conversation uses, where
       the native row can open a second step. Its panel is portaled to the body,
       so the rows are reached from `screen` rather than through the dialog. */
    const trigger = within(dialog).getByRole("button", { name: /^搜索提供商：/ });
    expect(trigger).toHaveTextContent("跟随对话设置");
    await user.click(trigger);
    const menu = screen.getByRole("menu", { name: "搜索提供商" });
    // A provider that is switched off is not a choice: offering it greyed out
    // would make the menu a list of things that do not work.
    expect(within(menu).queryByRole("menuitemradio", { name: /Exa/ })).not.toBeInTheDocument();
    // A role picks which backend searches, never which wire spelling that
    // backend uses, so the native row here has no second step under it.
    expect(within(menu).getByRole("menuitemradio", { name: "原生" }))
      .not.toHaveAttribute("aria-haspopup");
    // Naming no backend is an answer of its own, distinct from a binding that
    // broke: this role simply does not search.
    expect(within(menu).getByRole("menuitemradio", { name: "不启用" })).toBeInTheDocument();
    await user.click(within(menu).getByRole("menuitemradio", { name: "Tavily" }));
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange.mock.calls.at(-1)![0][0].searchProvider)
      .toEqual({ kind: "explicit", providerKind: "tavily" });
  });

  /* The two numbers deliberately have no "follow the conversation" row the way
     the backend above does: 0 is already the "no cap" answer, so there is no
     value left over to spell inheritance with, and a role always answers for
     itself. */
  it("carries its own result shaping, and takes 0 as no limit", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(
      within(dialog).getByRole("textbox", { name: "角色名称" }),
      "shaper"
    );
    await openPage(user, dialog, /^工具/);

    const resultCount = within(dialog).getByRole("spinbutton", { name: "结果数" });
    const compression = within(dialog).getByRole("spinbutton", { name: "结果压缩" });
    expect(resultCount).toHaveValue(DEFAULT_SEARCH_MAX_RESULTS);
    expect(compression).toHaveValue(DEFAULT_SEARCH_COMPRESSION_CUTOFF);

    await user.clear(resultCount);
    await user.type(resultCount, "12");
    await user.clear(compression);
    await user.type(compression, "0");
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    const saved = onChange.mock.calls.at(-1)![0][0];
    expect(saved.maxResults).toBe(12);
    expect(saved.compressionCutoff).toBe(0);
  });

  it("never offers orchestration tools in the role tool picker", async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await openPage(user, dialog, /^工具/);

    expect(within(dialog).getByText("读取文件")).toBeInTheDocument();
    expect(within(dialog).getByText("运行命令")).toBeInTheDocument();
    // Orchestration tools are absent rather than disabled because every child
    // template excludes them.
    for (const label of [
      "子代理工具（不应出现）",
      "发送消息工具（不应出现）",
      "追加任务工具（不应出现）",
      "待办事项工具（不应出现）",
      "工作流工具（不应出现）"
    ]) {
      expect(within(dialog).queryByText(label)).not.toBeInTheDocument();
    }
    for (const name of ["agent_spawn", "send_message", "followup_task", "todo", "workflow"]) {
      expect(dialog.querySelector(`[data-tool-name="${name}"]`)).not.toBeInTheDocument();
    }
    expect(dialog.querySelector('[data-tool-category="orchestration"]')).not.toBeInTheDocument();
    // Inherited conversation settings must not reintroduce orchestration tools.
    expect(within(dialog).getByText("跟随对话设置：本对话启用哪些，这个角色就有哪些"))
      .toBeInTheDocument();
    expect(dialog.querySelectorAll('[data-tool-name]')).toHaveLength(2);
  });

  it("counts only the tools it still shows when an allowlist names hidden ones", async () => {
    // Keep stale hidden names until a deliberate revision change; count only visible tools.
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderSettings(
      [userDefinition({ tools: ["agent_spawn", "read_file", "todo"] })],
      onChange
    );
    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    await openPage(user, dialog, /^工具/);

    expect(within(dialog).getByText("1 / 2 个已选")).toBeInTheDocument();

    // Changing a visible tool must preserve hidden stale names.
    await user.click(within(dialog).getByRole("button", { name: "运行命令已关闭" }));
    expect(within(dialog).getByText("2 / 2 个已选")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange.mock.calls.at(-1)![0][0].tools)
      .toEqual(["agent_spawn", "read_file", "run_command", "todo"]);
  });

  it("opens a new role on inherit for everything it can follow the caller on", async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });

    expect(within(dialog).getByRole("combobox", { name: "执行模型" })).toHaveValue("inherit");
    expect(within(dialog).getByRole("combobox", { name: "推理强度" })).toHaveValue("inherit");
    await openPage(user, dialog, /^工具/);
    expect(within(dialog).getByRole("button", { name: "搜索提供商：跟随对话设置" }))
      .toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "抓取提供商：跟随对话设置" }))
      .toBeInTheDocument();
    expect(within(dialog).getByRole("combobox", { name: "域名过滤" })).toHaveValue("inherit");
    // Tools too: the heading says it is following rather than reporting a count.
    expect(within(dialog).getByText("跟随对话设置：本对话启用哪些，这个角色就有哪些"))
      .toBeInTheDocument();
  });

  it("saves a role's own fetch backend and domain rules", async () => {
    const user = userEvent.setup();
    const existing = userDefinition();
    const onChange = renderSettings([existing]);
    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "reviewer" });
    await openPage(user, dialog, /^工具/);

    await user.click(within(dialog).getByRole("button", { name: "抓取提供商：跟随对话设置" }));
    await user.click(within(screen.getByRole("menu", { name: "抓取提供商" }))
      .getByRole("menuitemradio", { name: "Jina" }));
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "域名过滤" }),
      within(dialog).getByRole("option", { name: "启用白名单" })
    );
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    const saved = onChange.mock.calls.at(-1)![0][0];
    expect(saved.fetchProvider).toEqual({ kind: "explicit", providerKind: "jina" });
    expect(saved.domainFilter).toBe("include");
    expect(saved.includeDomains).toEqual([]);
    // Both are capability-bearing, so the identity ledger has to move with them.
    expect(saved.revision).toBe(existing.revision + 1);
  });

  it("keeps a half-written role until it is saved, and keeps each list's separate", async () => {
    const user = userEvent.setup();
    renderSettings([], vi.fn(), [], "roles-draft-a");
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    await user.type(
      within(screen.getByRole("dialog", { name: "新建角色" })).getByRole("textbox", { name: "角色名称" }),
      "半成品"
    );
    // Closing is not discarding: nothing has been written to the role either
    // way, and leaving to look at the conversation's own tools is not a
    // decision to throw the form away.
    await user.click(within(screen.getByRole("dialog", { name: "新建角色" }))
      .getByRole("button", { name: "关闭" }));
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    expect(within(screen.getByRole("dialog", { name: "新建角色" }))
      .getByRole("textbox", { name: "角色名称" })).toHaveValue("半成品");

    // A second conversation's list is a second drawer, not the same one.
    cleanup();
    renderSettings([], vi.fn(), [], "roles-draft-b");
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    expect(within(screen.getByRole("dialog", { name: "新建角色" }))
      .getByRole("textbox", { name: "角色名称" })).toHaveValue("");
  });

  it("drops the draft once it has become the role", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings([], vi.fn(), [], "roles-draft-saved");
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(within(dialog).getByRole("textbox", { name: "角色名称" }), "已保存");
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));
    expect(onChange).toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "新建角色" }));
    expect(within(screen.getByRole("dialog", { name: "新建角色" }))
      .getByRole("textbox", { name: "角色名称" })).toHaveValue("");
  });

  it("keeps active read-only definitions while hiding them and tombstones on save", async () => {
    const user = userEvent.setup();
    const tombstone = userDefinition({ name: "deleted-reviewer", deleted: true });
    const managed = userDefinition({ name: "managed-reviewer", source: "managed", revision: 9 });
    const onChange = renderSettings([tombstone, managed]);

    expect(screen.queryByText("deleted-reviewer")).not.toBeInTheDocument();
    expect(screen.queryByText("managed-reviewer")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });
    await user.type(within(dialog).getByRole("textbox", { name: "角色名称" }), "writer");
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange).toHaveBeenCalledWith([
      managed,
      expect.objectContaining({ name: "writer", revision: 1, memoryEpoch: 1 })
    ]);
  });

  it("requires a second click on the row's own button before deleting a role", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings([userDefinition()]);

    await user.click(screen.getByRole("button", { name: "删除角色 reviewer" }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "确认删除角色 reviewer" }));
    expect(onChange).toHaveBeenCalledWith([]);
  });
});
