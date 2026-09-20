import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, {
  mergeResourceOrder,
  reorderDocumentResources
} from "./App";
import { createTestDocument as createSeedDocument } from "./test/fixtures";
import { configureI18n } from "./i18n";
import type {
  AppDocument,
  Conversation,
  ResourceDescriptor
} from "./types";
import { resetAppMocks, browserRendererMountMocks, documentWithModel, runtimeMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/terminal", async () => (await import("./test/appMockInstances")).terminalMocks);
vi.mock("./lib/browser", async () => (await import("./test/appMockInstances")).browserMocks);
vi.mock("./lib/browserRendererMount", async () => {
  const { browserRendererMountMocks } = await import("./test/appMockInstances");
  return {
    startBrowserRendererMountHeartbeat: browserRendererMountMocks.startHeartbeat,
    stopBrowserRendererMountHeartbeat: browserRendererMountMocks.stopHeartbeat
  };
});
vi.mock("./lib/git", async (importOriginal) => {
  const { gitMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/git")>(), ...gitMocks };
});
vi.mock("./components/TerminalPanel", async () => (await import("./test/appMockInstances")).terminalPanelModuleMock());

afterEach(() => configureI18n("zh-CN"));

describe("App model run flow — capabilities", () => {
  beforeEach(resetAppMocks);

  it("keeps one native renderer mount heartbeat for the App lifetime", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    runtimeMocks.loadDocument.mockResolvedValue(createSeedDocument());
    const view = render(<App />);

    await waitFor(() => {
      expect(browserRendererMountMocks.startHeartbeat).toHaveBeenCalledTimes(1);
    });
    view.unmount();

    expect(browserRendererMountMocks.stopHeartbeat).toHaveBeenCalledTimes(1);
  });

  it("reorders the capability catalog without touching direct resource references", () => {
    const document = createSeedDocument();
    const template = document.capabilities.skills[0];
    document.capabilities.hooks = ["hook-a", "hook-b", "hook-c"].map((id) => ({
      ...template,
      id,
      name: id
    }));
    const conversation = document.workspaces[0].conversations[0];
    conversation.settings.hookIds = ["hook-a", "hook-c"];
    document.globalSettings.conversationPresets[0].settings.hookIds = ["hook-b", "hook-a"];

    const reordered = reorderDocumentResources(document, "hooks", "hook-c", "hook-a", "before");

    expect(reordered.capabilities.hooks.map((resource) => resource.id)).toEqual(["hook-c", "hook-a", "hook-b"]);
    // Catalog order is display-only: conversations and presets retain direct resource IDs.
    expect(reordered.workspaces[0].conversations[0].settings.hookIds).toEqual(["hook-a", "hook-c"]);
    expect(reordered.globalSettings.conversationPresets[0].settings.hookIds).toEqual(["hook-b", "hook-a"]);
  });

  it("keeps a custom resource order when a scan updates entries and adds new ones", () => {
    const descriptor = (id: string, name: string): ResourceDescriptor => ({
      id,
      name,
      description: "",
      location: `test://skills/${id}/SKILL.md`,
      source: "user",
      available: true
    });
    const first = descriptor("skill-a", "技能 A");
    const second = descriptor("skill-b", "技能 B");
    const merged = mergeResourceOrder(
      [second, first],
      [{ ...first, description: "已更新" }, { ...second, description: "也已更新" }, descriptor("skill-new", "新技能")]
    );

    expect(merged.map((resource) => resource.id)).toEqual([second.id, first.id, "skill-new"]);
    expect(merged[1].description).toBe("已更新");
  });

  /**
   * Skills, MCP servers and hooks are files the user owns, so a delete is a host
   * command naming the catalog id — never a write to a renderer-side registry,
   * which no longer exists — and the catalog is re-read afterwards rather than
   * patched, because every id is a hash over the entry's place on disk.
   */
  it("deletes a skill through the host command and republishes the rescanned catalog", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    /* The scan answers what is on disk at the moment it runs: the skill is there
       until the delete removes it, which is exactly what makes the row's
       disappearance evidence that the host did the work. */
    runtimeMocks.refreshCapabilities.mockResolvedValue(document.capabilities);
    runtimeMocks.deleteSkill.mockImplementation(async () => {
      runtimeMocks.refreshCapabilities.mockResolvedValue({
        ...document.capabilities,
        skills: []
      });
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "对话设置" }));
    const settings = await screen.findByRole("region", { name: "对话设置" });
    // Opening the pane is one of the moments discovery runs.
    await waitFor(() => expect(runtimeMocks.refreshCapabilities).toHaveBeenCalled());

    const navigation = within(settings).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(navigation).getByRole("button", { name: /^技能/ }));
    const row = within(settings).getByText("代码审查").closest(".catalog-row") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: "删除 代码审查" }));
    await user.click(within(row).getByRole("button", { name: "确认删除 代码审查" }));

    await waitFor(() => expect(runtimeMocks.deleteSkill).toHaveBeenCalledWith("skill_code_review"));
    // The row leaves because the rescan says it is gone, not because the renderer removed it.
    await waitFor(() => {
      expect(within(settings).queryByText("代码审查")).not.toBeInTheDocument();
    });
  });

  it("opens a capability directory and probes an MCP server by id", async () => {
    const document = documentWithModel();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.refreshCapabilities.mockResolvedValue(document.capabilities);
    runtimeMocks.probeMcpServer.mockResolvedValue({
      ok: true,
      cancelled: false,
      protocolVersion: "2025-06-18",
      serverName: "Workspace Files",
      serverVersion: "1.0.0",
      tools: [{ name: "read", title: "", description: "", requiresUserInteraction: false, inputSchema: {} }],
      prompts: [],
      resources: [],
      logs: [],
      error: ""
    });
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "对话设置" }));
    const settings = await screen.findByRole("region", { name: "对话设置" });
    const navigation = within(settings).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(navigation).getByRole("button", { name: /^MCP/ }));

    await user.click(within(settings).getByRole("button", { name: "打开全局配置目录" }));
    // The global level is the host's `workspace_id: None`, which the wrapper sends as absent.
    await waitFor(() => expect(runtimeMocks.revealCapabilityLocation).toHaveBeenCalledWith("mcp", undefined));

    const row = within(settings).getByText("Workspace Files").closest(".catalog-row") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: "测试连接 Workspace Files" }));
    // The renderer names a server; the host re-reads its configuration from disk.
    await waitFor(() => expect(runtimeMocks.probeMcpServer).toHaveBeenCalledWith(
      "mcp_workspace",
      expect.stringMatching(/^mcp-probe_/u)
    ));
  });
});

/**
 * Selecting more than one capability in a single conversation.
 *
 * Every click is a whole-settings write that travels the command channel, and
 * the host answers each one with the body it committed, which is then applied
 * back over the renderer's copy. So a second selection has to survive two
 * separate races: another click arriving inside the 250 ms debounce window, and
 * the previous commit's authoritative reply landing between the two clicks. The
 * default `updateConversationRemote` mock resolves `null` — no reply at all —
 * which is exactly the arrangement that cannot see either race, hence the
 * host-shaped mock below.
 */
describe("App conversation settings — selecting several capabilities", () => {
  beforeEach(resetAppMocks);

  /** A global (workspace-less) catalog entry, the only kind every conversation may select. */
  const resource = (id: string, name: string): ResourceDescriptor => ({
    id,
    name,
    description: "测试用条目。",
    location: `test://${id}`,
    source: "user",
    available: true
  });

  /**
   * An App wired to a host that owns conversation bodies.
   *
   * `updateConversationRemote` answers with the body it was handed, which is
   * what the real `update_conversation` does and what makes
   * `applyAuthoritativeConversation` a participant in the flow rather than a
   * no-op. `committed` is the last body the host accepted.
   */
  function appWithConversationHost(conversationId?: string) {
    const document: AppDocument = createSeedDocument();
    document.capabilities.skills.push(resource("skill_release", "发布流程"));
    document.capabilities.mcps.push(resource("mcp_second", "Second Server"));
    const conversation = document.workspaces[0].conversations[0];
    // A per-test id so a stale commit from an earlier test's App is tellable apart.
    if (conversationId) conversation.id = conversationId;
    // Start from nothing selected: the report is about newly enabling entries.
    conversation.settings.skillIds = [];
    conversation.settings.mcpIds = [];
    // Nothing was ever sent here, so no run has locked a tool surface and the
    // host's context sequence matches the renderer's empty one.
    conversation.contexts = [];
    document.workspaces[0].conversations = [conversation];

    const host = { committed: conversation };
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.refreshCapabilities.mockResolvedValue(document.capabilities);
    runtimeMocks.hasConversationCommands.mockReturnValue(true);
    runtimeMocks.createConversationRemote.mockImplementation(async (_workspaceId, next) => next);
    runtimeMocks.loadConversationRemote.mockImplementation(async () => host.committed);
    runtimeMocks.updateConversationRemote.mockImplementation(async (
      _workspaceId: string,
      next: Conversation
    ) => {
      // Crossing the IPC boundary copies the body, so the reply is structurally
      // equal but never the same object the renderer sent. Reproduce that: an
      // identity-preserving mock would make `applyAuthoritativeConversation` a
      // no-op for React and hide a reply that overwrites newer state.
      const stored = JSON.parse(JSON.stringify(next)) as Conversation;
      host.committed = stored;
      return stored;
    });
    return { document, host };
  }

  /** An App whose only conversation is the renderer-side draft: no workspace, nothing sent. */
  function appWithDraftConversation() {
    const document: AppDocument = createSeedDocument();
    document.capabilities.skills.push(resource("skill_release", "发布流程"));
    document.capabilities.mcps.push(resource("mcp_second", "Second Server"));
    // No workspace at all, which is the one arrangement that opens a draft the
    // renderer owns outright — `openDraftConversation` otherwise makes the new
    // task a real conversation immediately.
    document.workspaces = [];
    for (const preset of document.globalSettings.conversationPresets) {
      preset.settings.skillIds = [];
      preset.settings.mcpIds = [];
    }
    runtimeMocks.loadDocument.mockResolvedValue(document);
    runtimeMocks.refreshCapabilities.mockResolvedValue(document.capabilities);
    return { document };
  }

  async function openCapabilityPage(
    user: ReturnType<typeof userEvent.setup>,
    page: RegExp
  ): Promise<HTMLElement> {
    await user.click(screen.getByRole("button", { name: "更多选项" }));
    await user.click(await screen.findByRole("menuitemradio", { name: "对话设置" }));
    const settings = await screen.findByRole("region", { name: "对话设置" });
    await waitFor(() => expect(runtimeMocks.refreshCapabilities).toHaveBeenCalled());
    const navigation = within(settings).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(navigation).getByRole("button", { name: page }));
    return settings;
  }

  /** Lets the 250 ms commit debounce fire and its reply be applied. */
  async function settleCommit(): Promise<void> {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 320));
    });
  }

  /** The ids in the last body the host was asked to commit. */
  function lastCommitted(field: "skillIds" | "mcpIds"): string[] {
    const calls = runtimeMocks.updateConversationRemote.mock.calls;
    expect(calls.length).toBeGreaterThan(0);
    return (calls.at(-1)![1] as Conversation).settings[field];
  }

  it("keeps both skills when the second click lands inside the commit debounce window", async () => {
    appWithConversationHost();
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^技能/);

    await user.click(within(settings).getByRole("switch", { name: /代码审查/ }));
    await user.click(within(settings).getByRole("switch", { name: /发布流程/ }));

    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
    await settleCommit();
    expect(lastCommitted("skillIds")).toEqual(["skill_code_review", "skill_release"]);
    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
  });

  it("keeps both skills when the first commit and its authoritative reply land in between", async () => {
    appWithConversationHost();
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^技能/);

    await user.click(within(settings).getByRole("switch", { name: /代码审查/ }));
    await settleCommit();
    expect(lastCommitted("skillIds")).toEqual(["skill_code_review"]);
    // The row the user reaches for next must still be live after the reply.
    const second = within(settings).getByRole("switch", { name: /发布流程/ });
    expect(second).toBeEnabled();
    await user.click(second);

    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
    await settleCommit();
    expect(lastCommitted("skillIds")).toEqual(["skill_code_review", "skill_release"]);
  });

  /**
   * The ordering the two above cannot see: the first commit has already left
   * for the host, and the user clicks the second row while its reply is still
   * on the wire. That reply describes the conversation as it was one click ago,
   * and it is applied over the renderer's copy unconditionally.
   *
   * Writes are matched by the body they carry rather than by call order: an App
   * from an earlier test in this file can still own a conversation-sync timer,
   * and its late commit would otherwise be mistaken for this one's.
   */
  function deferredHostWrites(conversationId: string) {
    const writes: { body: Conversation; resolve: (stored: Conversation) => void }[] = [];
    runtimeMocks.updateConversationRemote.mockImplementation((
      _workspaceId: string,
      next: Conversation
    ) => new Promise<Conversation>((resolve) => {
      const body = JSON.parse(JSON.stringify(next)) as Conversation;
      if (body.id === conversationId) writes.push({ body, resolve });
      else resolve(body);
    }));
    /** Waits for the write whose skill selection is exactly `skillIds`. */
    const write = async (skillIds: string[]) => {
      await waitFor(() => expect(writes.some((candidate) => (
        candidate.body.settings.skillIds.join() === skillIds.join()
      ))).toBe(true));
      return writes.find((candidate) => candidate.body.settings.skillIds.join() === skillIds.join())!;
    };
    return { writes, write };
  }

  it("keeps the second skill when the first commit's reply is still in flight", async () => {
    const { document } = appWithConversationHost("conv_inflight");
    const conversation = document.workspaces[0].conversations[0];
    const host = deferredHostWrites(conversation.id);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^技能/);

    await user.click(within(settings).getByRole("switch", { name: /代码审查/ }));
    // Wait out the debounce so the first write is on the wire, unanswered.
    const first = await host.write(["skill_code_review"]);

    await user.click(within(settings).getByRole("switch", { name: /发布流程/ }));
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();

    // The host answers the FIRST write now, with the body it committed for it.
    await act(async () => { first.resolve(first.body); });

    // A reply about an older selection must not take the newer one off screen.
    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
  });

  /**
   * The other half of the sequence above: the selection the reply did not take
   * off screen is also the one that reaches the host. The queued second commit
   * carries both ids, so the row stays on from the click until the write
   * settles — no flicker in between, and every further click lands the same way.
   */
  it("commits the newer selection it kept on screen", async () => {
    const { document } = appWithConversationHost("conv_recommit");
    const conversation = document.workspaces[0].conversations[0];
    const host = deferredHostWrites(conversation.id);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^技能/);

    await user.click(within(settings).getByRole("switch", { name: /代码审查/ }));
    const first = await host.write(["skill_code_review"]);
    await user.click(within(settings).getByRole("switch", { name: /发布流程/ }));
    await act(async () => { first.resolve(first.body); });
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();

    const second = await host.write(["skill_code_review", "skill_release"]);
    await act(async () => { second.resolve(second.body); });
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
  });

  it("keeps both MCP servers whichever side of the commit the second click falls on", async () => {
    appWithConversationHost();
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^MCP/);

    await user.click(within(settings).getByRole("switch", { name: /Workspace Files/ }));
    await user.click(within(settings).getByRole("switch", { name: /Second Server/ }));
    await settleCommit();
    expect(lastCommitted("mcpIds")).toEqual(["mcp_workspace", "mcp_second"]);
    expect(within(settings).getByRole("switch", { name: /Workspace Files/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /Second Server/ })).toBeChecked();

    // And once more across a settled commit, from two selections to one removal
    // and back: the pane must stay editable rather than freeze on its first write.
    await user.click(within(settings).getByRole("switch", { name: /Workspace Files/ }));
    await settleCommit();
    expect(lastCommitted("mcpIds")).toEqual(["mcp_second"]);
    await user.click(within(settings).getByRole("switch", { name: /Workspace Files/ }));
    await settleCommit();
    expect(lastCommitted("mcpIds")).toEqual(["mcp_second", "mcp_workspace"]);
  });

  /**
   * The draft the app opens with when there is no workspace to persist into. Its
   * settings live in React state rather than in the document, so no host reply
   * can overwrite them — but a document republication (a capability rescan is
   * one, and opening the pane triggers one) must not re-derive them either.
   */
  it("keeps both skills and both MCP servers on a draft conversation", async () => {
    appWithDraftConversation();
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const settings = await openCapabilityPage(user, /^技能/);

    await user.click(within(settings).getByRole("switch", { name: /代码审查/ }));
    await user.click(within(settings).getByRole("switch", { name: /发布流程/ }));
    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();

    // A rescan republishes the document; the draft is not part of it and must not move.
    await user.click(within(settings).getByRole("button", { name: "重新扫描" }));
    await settleCommit();
    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();

    const navigation = within(settings).getByRole("navigation", { name: "对话设置分类" });
    await user.click(within(navigation).getByRole("button", { name: /^MCP/ }));
    await user.click(within(settings).getByRole("switch", { name: /Workspace Files/ }));
    await user.click(within(settings).getByRole("switch", { name: /Second Server/ }));
    await settleCommit();
    expect(within(settings).getByRole("switch", { name: /Workspace Files/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /Second Server/ })).toBeChecked();
    // And the skills the other page selected are still selected.
    await user.click(within(navigation).getByRole("button", { name: /^技能/ }));
    expect(within(settings).getByRole("switch", { name: /代码审查/ })).toBeChecked();
    expect(within(settings).getByRole("switch", { name: /发布流程/ })).toBeChecked();
  });
});
