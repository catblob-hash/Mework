import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createTestDocument } from "../../test/fixtures";
import {
  CLAUDE_AGENT_LEGAL_URL,
  CLAUDE_AGENT_LOGIN_COMMAND,
  CLAUDE_AGENT_REGISTRY
} from "../../lib/claudeAgentProvider";
import { normalizeDocument } from "../../lib/runtime";
import type {
  ApiProvider,
  ClaudeAgentLoginStatus,
  GlobalSettings as GlobalSettingsType
} from "../../types";
import { GlobalSettings } from "../GlobalSettings";

const runtimeMocks = vi.hoisted(() => ({
  deleteApiKey: vi.fn(),
  saveApiKey: vi.fn(),
  getStoredApiKeyLength: vi.fn(),
  forgetStoredApiKeyLength: vi.fn(),
  revealApiKey: vi.fn(),
  fetchModels: vi.fn(),
  claudeAgentLoginStatus: vi.fn(),
  claudeAgentOpenLogin: vi.fn()
}));

// Only the credential/discovery/login calls are stubbed; the fixtures still need
// the real defaults helpers this module also exports.
vi.mock("../../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/runtime")>()),
  ...runtimeMocks
}));

import { ClaudeAgentLoginPanel } from "./ClaudeAgentLoginPanel";

// Discovery delegates to the unmocked module so the assertion covers the real
// preview catalog rather than a list the test wrote itself.
const actualRuntime = await vi.importActual<typeof import("../../lib/runtime")>("../../lib/runtime");

const signedOut: ClaudeAgentLoginStatus = {
  signedIn: false,
  authMethod: "none",
  email: null,
  orgName: null,
  subscriptionType: null,
  executable: "claude",
  configDir: "/home/me/.claude"
};

const signedIn: ClaudeAgentLoginStatus = {
  signedIn: true,
  authMethod: "claude.ai",
  email: "person@example.com",
  orgName: "Example Org",
  subscriptionType: "max",
  executable: "/home/me/.local/bin/claude",
  configDir: "/home/me/.claude"
};

function renderProviders() {
  // Normalization is what puts the built-in rows in place, so the fixture goes
  // through it rather than hand-placing a Claude Agent row the product owns.
  const document = normalizeDocument(createTestDocument());
  let currentSettings = document.globalSettings;
  function Harness() {
    const [settings, setSettings] = useState<GlobalSettingsType>(document.globalSettings);
    currentSettings = settings;
    return (
      <GlobalSettings
        initialView="providers"
        settings={settings}
        tools={document.tools}
        capabilities={document.capabilities}
        workspaces={document.workspaces}
        onChange={setSettings}
        onFlush={vi.fn(() => Promise.resolve())}
        onClose={vi.fn()}
      />
    );
  }
  return { ...render(<Harness />), getSettings: () => currentSettings };
}

function claudeAgentProvider(): ApiProvider {
  return {
    id: "provider_claude_agent",
    name: "Claude Agent",
    enabled: false,
    family: "claude_agent",
    baseUrl: "",
    familySettings: {},
    endpointBaseUrls: {},
    notes: "",
    models: [],
    activeModelId: null
  };
}

function renderPanel(overrides: Partial<ComponentProps<typeof ClaudeAgentLoginPanel>> = {}) {
  const onSignedInChange = vi.fn();
  return {
    onSignedInChange,
    ...render(<ClaudeAgentLoginPanel
      provider={claudeAgentProvider()}
      desktopRuntime
      onSignedInChange={onSignedInChange}
      {...overrides}
    />)
  };
}

/**
 * The Claude Agent family is a built-in row like Codex: normalization ensures
 * exactly one, it is not offered in the add dialog, and it cannot be deleted or
 * repurposed into another family.
 */
describe("Claude Agent provider", () => {
  beforeEach(() => {
    runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
    runtimeMocks.saveApiKey.mockReset().mockResolvedValue({ configured: true, keyLength: 8 });
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.revealApiKey.mockReset().mockResolvedValue("stored-secret-key");
    runtimeMocks.fetchModels.mockReset().mockImplementation(actualRuntime.fetchModels);
    runtimeMocks.claudeAgentLoginStatus.mockReset().mockResolvedValue(signedOut);
    runtimeMocks.claudeAgentOpenLogin.mockReset().mockResolvedValue(undefined);
  });

  it("is a built-in row that the add dialog does not offer", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    expect(getSettings().apiProviders.filter((provider) => provider.family === "claude_agent"))
      .toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "添加提供商" }));
    const dialog = screen.getByRole("dialog", { name: "添加提供商" });
    const protocols = within(dialog).getByLabelText("对话协议");
    expect(within(protocols).queryByRole("option", { name: /Claude Agent/u })).not.toBeInTheDocument();
  });

  it("cannot be deleted or repurposed into another family", async () => {
    const user = userEvent.setup();
    renderProviders();

    await user.click(screen.getByRole("button", { name: "Claude Agent" }));
    // The built-in row carries no kebab, so there is no route to the delete item.
    expect(screen.queryByRole("button", { name: "Claude Agent 的更多操作" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "提供商设置" }));
    const drawer = screen.getByRole("dialog", { name: "提供商设置" });
    const protocol = within(drawer).getByLabelText("API 格式");
    expect(protocol).toBeDisabled();
    expect(within(protocol).getByRole("option", { name: "Claude Agent (Claude Code)" })).toBeInTheDocument();
    expect(within(protocol).queryByRole("option", { name: "Anthropic Messages" })).not.toBeInTheDocument();
  });

  it("offers neither a key nor an address, only the local CLI path", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "Claude Agent" }));

    expect(getSettings().apiProviders.find((provider) => provider.family === "claude_agent"))
      .toMatchObject({
        name: "Claude Agent",
        family: "claude_agent",
        // The CLI owns both the credential and the endpoint.
        baseUrl: "",
        enabled: false
      });

    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Base URL")).not.toBeInTheDocument();
    expect(screen.queryByText("API 地址")).not.toBeInTheDocument();
    // The compliance stance is on the panel itself, not buried in a drawer.
    expect(screen.getByRole("link", { name: "Claude Code 使用条款" }))
      .toHaveAttribute("href", CLAUDE_AGENT_LEGAL_URL);

    // `knownFamilySettings` drives the identity section: the executable path shows up.
    await user.click(screen.getByRole("button", { name: "提供商设置" }));
    const drawer = screen.getByRole("dialog", { name: "提供商设置" });
    const executable = within(drawer).getByLabelText("Claude Code 路径");
    expect(executable).toHaveAttribute("placeholder", "留空 = 自动查找 ~/.local/bin 与 PATH");
    // It is optional, so the label carries no required marker.
    expect(within(drawer).queryByText("Claude Code 路径 *")).not.toBeInTheDocument();
    // No address of any kind, not even the non-chat endpoints.
    expect(within(drawer).queryByText("端点地址")).not.toBeInTheDocument();

    await user.type(executable, "C:\\Users\\me\\.local\\bin\\claude.exe");
    expect(getSettings().apiProviders.find((provider) => provider.family === "claude_agent")!.familySettings)
      .toEqual({ claude_executable: "C:\\Users\\me\\.local\\bin\\claude.exe" });
  });

  it("tells the browser preview it cannot read the sign-in status", async () => {
    const user = userEvent.setup();
    renderProviders();

    await user.click(screen.getByRole("button", { name: "Claude Agent" }));

    expect(screen.getByText("浏览器预览无法读取 Claude Code 登录状态。")).toBeInTheDocument();
    // Asking a preview would only produce a bridge error.
    expect(runtimeMocks.claudeAgentLoginStatus).not.toHaveBeenCalled();
  });

  it("discovers the built-in registry instead of an upstream catalog", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "Claude Agent" }));
    await user.click(screen.getByRole("button", { name: "拉取模型" }));

    // The discovery page lists every registry row; installing all of them is
    // what turns the list into the provider's models.
    expect(await screen.findByRole("button", { name: "添加到提供商 claude-opus-5[1m]" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "添加全部结果" }));

    await waitFor(() => expect(
      getSettings().apiProviders.find((provider) => provider.family === "claude_agent")!.models
    ).toHaveLength(CLAUDE_AGENT_REGISTRY.length));

    const models = getSettings().apiProviders.find((provider) => provider.family === "claude_agent")!.models;
    expect(models.map((model) => model.id).sort()).toEqual(CLAUDE_AGENT_REGISTRY.map((entry) => entry.id).sort());
    // The `[1m]` twins are separate rows with the larger window, not duplicates.
    expect(models.find((model) => model.id === "claude-opus-5")).toMatchObject({ contextWindow: 200000 });
    expect(models.find((model) => model.id === "claude-opus-5[1m]")).toMatchObject({
      name: "Claude Opus 5 (1M context)",
      contextWindow: 1000000
    });
    expect((await screen.findAllByText("Claude Opus 5 (1M context)")).length).toBeGreaterThan(0);
  });
});

describe("ClaudeAgentLoginPanel", () => {
  beforeEach(() => {
    runtimeMocks.claudeAgentLoginStatus.mockReset().mockResolvedValue(signedOut);
    runtimeMocks.claudeAgentOpenLogin.mockReset().mockResolvedValue(undefined);
  });

  it("shows the sign-in command while the CLI is signed out", async () => {
    const user = userEvent.setup();
    renderPanel();

    expect(await screen.findByText(CLAUDE_AGENT_LOGIN_COMMAND)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "打开终端登录" }));
    await waitFor(() => expect(runtimeMocks.claudeAgentOpenLogin)
      .toHaveBeenCalledWith(expect.objectContaining({ id: "provider_claude_agent" })));
  });

  it("shows who the CLI is signed in as", async () => {
    runtimeMocks.claudeAgentLoginStatus.mockResolvedValue(signedIn);
    renderPanel();

    expect(await screen.findByText("已登录 Claude Code")).toBeInTheDocument();
    expect(screen.getByText("person@example.com · Example Org · Max")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "打开终端登录" })).not.toBeInTheDocument();
  });

  /// A Console login is billed per token, which is a materially different deal
  /// from a subscription and must not read the same.
  it("names a Console login as API billing", async () => {
    runtimeMocks.claudeAgentLoginStatus.mockResolvedValue({
      ...signedIn,
      authMethod: "console",
      orgName: null,
      subscriptionType: null
    });
    renderPanel();

    expect(await screen.findByText("person@example.com · Console（API 计费）")).toBeInTheDocument();
  });

  it("enables the provider when a re-check finds a completed sign-in", async () => {
    const user = userEvent.setup();
    runtimeMocks.claudeAgentLoginStatus.mockResolvedValueOnce(signedOut).mockResolvedValue(signedIn);
    const { onSignedInChange } = renderPanel();

    await screen.findByRole("button", { name: "打开终端登录" });
    expect(onSignedInChange).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "重新检查" }));
    expect(await screen.findByText("已登录 Claude Code")).toBeInTheDocument();
    await waitFor(() => expect(onSignedInChange).toHaveBeenCalledWith(true));
  });

  /// An already-signed-in status on first load is not a transition: the user may
  /// have disabled the row deliberately, so the panel must not re-enable it.
  it("does not touch the enabled flag for a CLI that was already signed in", async () => {
    runtimeMocks.claudeAgentLoginStatus.mockResolvedValue(signedIn);
    const { onSignedInChange } = renderPanel();

    expect(await screen.findByText("已登录 Claude Code")).toBeInTheDocument();
    expect(onSignedInChange).not.toHaveBeenCalled();
  });

  it("offers a retry and points at the executable path when the read fails", async () => {
    const user = userEvent.setup();
    runtimeMocks.claudeAgentLoginStatus
      .mockRejectedValueOnce(new Error("找不到 claude 可执行文件"))
      .mockResolvedValue(signedOut);
    renderPanel();

    expect(await screen.findByRole("alert")).toHaveTextContent("找不到 claude 可执行文件");
    expect(screen.getByText("可以在「提供商设置」里填 Claude Code 路径。")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("button", { name: "打开终端登录" })).toBeEnabled();
  });

  it("does not ask the host in a browser preview", async () => {
    renderPanel({ desktopRuntime: false });

    expect(await screen.findByText("浏览器预览无法读取 Claude Code 登录状态。")).toBeInTheDocument();
    expect(runtimeMocks.claudeAgentLoginStatus).not.toHaveBeenCalled();
  });

  it("flushes pending document edits before every host call", async () => {
    const user = userEvent.setup();
    const onBeforeHostCall = vi.fn().mockResolvedValue(undefined);
    renderPanel({ onBeforeHostCall });

    await screen.findByRole("button", { name: "打开终端登录" });
    expect(onBeforeHostCall).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "打开终端登录" }));
    await waitFor(() => expect(runtimeMocks.claudeAgentOpenLogin).toHaveBeenCalledTimes(1));
    expect(onBeforeHostCall).toHaveBeenCalledTimes(2);
    expect(onBeforeHostCall.mock.invocationCallOrder[1]).toBeLessThan(
      runtimeMocks.claudeAgentOpenLogin.mock.invocationCallOrder[0]
    );
  });

  /// A copy that fails (the document lost focus) and is retried must leave only
  /// the outcome of the retry on screen: no stale alert beside a "copied" label.
  it("clears a copy failure once a retry succeeds", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn()
      .mockRejectedValueOnce(new Error("Document is not focused."))
      .mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    try {
      renderPanel();
      const copy = await screen.findByRole("button", { name: "复制命令" });

      await user.click(copy);
      expect(await screen.findByRole("alert")).toHaveTextContent("复制命令失败：Document is not focused.");
      expect(screen.queryByRole("button", { name: "已复制" })).not.toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: "复制命令" }));
      expect(await screen.findByRole("button", { name: "已复制" })).toBeInTheDocument();
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(writeText).toHaveBeenLastCalledWith(CLAUDE_AGENT_LOGIN_COMMAND);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

/**
 * The other built-in row. Its address left the main pane with this change; the
 * only remaining writer is the drawer field reserved for a local test double,
 * which the fake-backend e2e depends on.
 */
describe("OpenAI Codex provider address", () => {
  beforeEach(() => {
    runtimeMocks.deleteApiKey.mockReset().mockResolvedValue({ configured: false });
    runtimeMocks.saveApiKey.mockReset().mockResolvedValue({ configured: true, keyLength: 8 });
    runtimeMocks.getStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.forgetStoredApiKeyLength.mockReset().mockResolvedValue(undefined);
    runtimeMocks.revealApiKey.mockReset().mockResolvedValue("stored-secret-key");
    runtimeMocks.fetchModels.mockReset().mockResolvedValue([]);
    runtimeMocks.claudeAgentLoginStatus.mockReset().mockResolvedValue(signedOut);
    runtimeMocks.claudeAgentOpenLogin.mockReset().mockResolvedValue(undefined);
  });

  it("keeps the address out of the main pane and in the drawer as a test-stub field", async () => {
    const user = userEvent.setup();
    const { getSettings } = renderProviders();

    await user.click(screen.getByRole("button", { name: "OpenAI Codex" }));
    expect(screen.queryByLabelText("Base URL")).not.toBeInTheDocument();
    expect(screen.queryByText("API 地址")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "提供商设置" }));
    const drawer = screen.getByRole("dialog", { name: "提供商设置" });
    const stub = within(drawer).getByLabelText("本机测试桩地址");
    await user.type(stub, "http://127.0.0.1:1466/backend-api/codex");
    expect(getSettings().apiProviders.find((provider) => provider.family === "openai_codex")!.baseUrl)
      .toBe("http://127.0.0.1:1466/backend-api/codex");
  });
});
