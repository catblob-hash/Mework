import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type {
  AgentDefinition,
  ApiProvider,
  ToolDescriptor,
  WebSearchAssets
} from "../types";
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
    reasoningContent: "plaintext"
  }, {
    id: "second-model",
    name: "",
    group: "",
    capabilities: [],
    reasoningContent: "plaintext"
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
    reasoningContent: "plaintext"
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

// Include enabled and disabled options in the same selector.
const webSearchAssets: WebSearchAssets = {
  providers: [{
    kind: "tavily",
    enabled: true,
    searchApiHost: "",
    fetchApiHost: "",
    engines: [],
    basicAuthUsername: ""
  }],
  fetchProvider: null,
  maxResults: 5,
  excludeDomains: [],
  compression: { method: "none", cutoffLimit: 0 }
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
    ...overrides
  };
}

function renderSettings(
  definitions: readonly AgentDefinition[] = [],
  onChange = vi.fn()
) {
  render(
    <AgentDefinitionSettings
      definitions={definitions}
      providers={providers}
      tools={tools}
      conversationEnabledTools={conversationEnabledTools}
      webSearchAssets={webSearchAssets}
      onChange={onChange}
    />
  );
  return onChange;
}

afterEach(() => {
  cleanup();
  configureI18n("zh-CN");
});

describe("AgentDefinitionSettings", () => {
  it.each([
    ["zh-CN" as const, "新建角色", "角色名称", "保存角色"],
    ["en-US" as const, "New role", "Role name", "Save role"]
  ])("the plus row opens an empty, unsavable create dialog (%s)", async (
    language,
    createLabel,
    nameLabel,
    saveLabel
  ) => {
    configureI18n(language);
    const user = userEvent.setup();
    renderSettings();

    const create = screen.getByRole("button", { name: createLabel });
    expect(create).toBeInTheDocument();
    await user.click(create);

    const dialog = screen.getByRole("dialog", { name: createLabel });
    expect(within(dialog).getByRole("textbox", { name: nameLabel })).toHaveValue("");
    expect(within(dialog).getByRole("button", { name: saveLabel })).toBeDisabled();
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
      searchProvider: null
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

    expect(within(screen.getByRole("button", { name: "打开角色 live-role" }))
      .getByText(exactModelId)).toBeInTheDocument();
    expect(within(screen.getByRole("button", { name: "打开角色 gone-role" }))
      .getByText("removed-model · 模型已不可用，模型看不到这个角色")).toBeInTheDocument();
    expect(within(screen.getByRole("button", { name: "打开角色 inheriting" }))
      .getByText("跟随对话模型")).toBeInTheDocument();
  });

  it("renders a persisted unavailable role without naming the model it lost", async () => {
    // The load-time conversion discards the dead provider/model IDs, so the row
    // has nothing to print — and must not imply the role still works.
    const user = userEvent.setup();
    renderSettings([
      userDefinition({ name: "gone-role", modelSelection: { kind: "unavailable" } })
    ]);

    const row = screen.getByRole("button", { name: "打开角色 gone-role" });
    expect(within(row).getByText("原模型已不可用，模型看不到这个角色")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "设置角色 gone-role" }));
    const dialog = screen.getByRole("dialog", { name: "角色设置" });
    const select = within(dialog).getByRole("combobox", { name: "执行模型" }) as HTMLSelectElement;
    // Nothing is selected: the dead binding gets no option of its own, so the
    // dropdown cannot present it as a choice — and must not fall back to
    // It must not fall back to the inherit-model option, which is a different
    // working configuration.
    expect(select.value).toBe("");
    expect(within(dialog).queryByRole("option", { name: "（原模型已不可用）" })).toBeNull();
    expect(select).toBeInvalid();
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
    const dialog = screen.getByRole("dialog", { name: "角色设置" });
    const select = within(dialog).getByRole("combobox", { name: "执行模型" }) as HTMLSelectElement;
    expect(select).toBeInvalid();
    expect(within(dialog).getByText("disabled-provider · hidden-model（不可用）")).toBeInTheDocument();
    expect(within(dialog).getByText(
      "绑定的模型已不在该提供商的模型列表里（或该提供商被停用），这个角色暂时不能被调用；把它装回来，绑定就会恢复。"
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
    const dialog = screen.getByRole("dialog", { name: "角色设置" });
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

  it("supports all tool bulk actions and restores tools:null when inheriting", async () => {
    const user = userEvent.setup();
    const existing = userDefinition({ tools: ["run_command"] });
    const onChange = renderSettings([existing]);
    await user.click(screen.getByRole("button", { name: "设置角色 reviewer" }));
    const dialog = screen.getByRole("dialog", { name: "角色设置" });

    await user.click(within(dialog).getByRole("button", { name: "全部关闭" }));
    expect(within(dialog).getByText("0 / 2 个已选")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "全部启用" }));
    expect(within(dialog).getByText("2 / 2 个已选")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "继承对话设置" }));
    expect(within(dialog).getByText("跟随对话设置：本对话启用哪些，这个角色就有哪些"))
      .toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "继承对话设置" })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange).toHaveBeenCalledWith([{
      ...existing,
      revision: existing.revision + 1,
      tools: null
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

    expect(conversationEnabledTools).not.toContain("run_command");
    await user.click(within(dialog).getByRole("switch", { name: "运行命令已关闭" }));
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

    const select = within(dialog).getByRole("combobox", { name: "搜索提供商" });
    expect(select).toHaveValue("inherit");
    // Disabled entries stay visible so existing bindings can be repaired.
    expect(within(dialog).getByRole("option", { name: "Exa（未启用）" })).toBeInTheDocument();
    await user.selectOptions(
      select,
      within(dialog).getByRole("option", { name: "Tavily" })
    );
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange.mock.calls.at(-1)![0][0].searchProvider)
      .toEqual({ kind: "explicit", providerKind: "tavily" });
  });

  it("never offers orchestration tools in the role tool picker", async () => {
    const user = userEvent.setup();
    renderSettings();
    await user.click(screen.getByRole("button", { name: "新建角色" }));
    const dialog = screen.getByRole("dialog", { name: "新建角色" });

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
    const dialog = screen.getByRole("dialog", { name: "角色设置" });

    expect(within(dialog).getByText("1 / 2 个已选")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "全部启用" })).toBeEnabled();

    // Changing a visible tool must preserve hidden stale names.
    await user.click(within(dialog).getByRole("switch", { name: "运行命令已关闭" }));
    expect(within(dialog).getByText("2 / 2 个已选")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "全部启用" })).toBeDisabled();
    await user.click(within(dialog).getByRole("button", { name: "保存角色" }));

    expect(onChange.mock.calls.at(-1)![0][0].tools)
      .toEqual(["agent_spawn", "read_file", "run_command", "todo"]);
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

  it("requires confirmation before deleting a role", async () => {
    const user = userEvent.setup();
    const onChange = renderSettings([userDefinition()]);

    await user.click(screen.getByRole("button", { name: "删除角色 reviewer" }));
    const confirmation = screen.getByRole("dialog", { name: "删除角色？" });
    expect(within(confirmation).getByText("reviewer")).toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();

    await user.click(within(confirmation).getByRole("button", { name: "删除角色" }));
    expect(onChange).toHaveBeenCalledWith([]);
  });
});
