import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import * as previewApi from "./lib/preview";
import type { AppDocument } from "./types";
import { configureI18n } from "./i18n";
import { browserMocks, documentWithModel, resetAppMocks, runtimeMocks } from "./test/appMocks";

vi.mock("./lib/runtime", async (importOriginal) => {
  const { runtimeMocks } = await import("./test/appMockInstances");
  return { ...await importOriginal<typeof import("./lib/runtime")>(), ...runtimeMocks };
});
vi.mock("./lib/workspacePicker", async () => (await import("./test/appMockInstances")).workspacePickerMocks);
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

function devbox() {
  return {
    id: "machine-devbox",
    name: "devbox",
    host: "user@devbox.local",
    port: 0,
    identityFile: "",
    createdAt: "2026-01-01T00:00:00.000Z",
    updatedAt: "2026-01-01T00:00:00.000Z"
  };
}

/** A project whose second workspace is a directory on an SSH machine. */
function documentWithRemoteWorkspace(): AppDocument {
  const document = documentWithModel();
  document.globalSettings.executionEnvironments.sshMachines = [devbox()];
  document.workspaces[0].additionalWorkspaces = [
    { machine: { kind: "ssh", machineId: "machine-devbox" }, path: "/srv/api" }
  ];
  return document;
}

function toolbar(): HTMLElement {
  return window.document.querySelector(".pane-toolbar") as HTMLElement;
}

afterEach(() => configureI18n("zh-CN"));

describe("preview pages across a conversation's workspaces", () => {
  beforeEach(() => {
    resetAppMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    vi.spyOn(previewApi, "listPreviewConfigurations").mockResolvedValue({
      launchJsonPath: "/srv/api/.mework/launch.json",
      servers: [],
      malformed: []
    });
    vi.spyOn(previewApi, "listPreviewServers").mockResolvedValue([]);
  });

  it("keeps the top bar's button a plain toggle while there is one workspace", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const conversationId = (await runtimeMocks.loadDocument.mock.results[0].value as AppDocument)
      .workspaces[0].conversations[0].id;

    await user.click(within(toolbar()).getByRole("button", { name: "预览" }));

    expect(screen.queryByRole("menu", { name: "打开预览" })).not.toBeInTheDocument();
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalled());
    expect(browserMocks.openBrowser.mock.calls[0]?.[0]).toBe(conversationId);
    // Bound to its workspace before the page exists, so its first request leaves from there.
    expect(browserMocks.setBrowserPageNetwork).toHaveBeenCalledWith(conversationId, { conversationId });
  });

  /**
   * The host can refuse an open (a page it is still creating, a full set of live pages). The pane
   * used to stay open with nothing in it and the button pressed, so the next click closed it and
   * only the one after that opened the page.
   */
  it("closes the pane when the host refuses the open, so one click tries again", async () => {
    runtimeMocks.loadDocument.mockResolvedValue(documentWithModel());
    browserMocks.openBrowser.mockRejectedValueOnce(
      new Error("this browser task page is being created or restored; try again shortly")
    );
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    const preview = within(toolbar()).getByRole("button", { name: "预览" });

    await user.click(preview);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(preview).toHaveAttribute("aria-pressed", "false"));

    await user.click(preview);
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(preview).toHaveAttribute("aria-pressed", "true"));
  });

  it("opens a page for the workspace picked from the top bar, on that workspace's machine", async () => {
    const document = documentWithRemoteWorkspace();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversationId = document.workspaces[0].conversations[0].id;
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");

    await user.click(within(toolbar()).getByRole("button", { name: "预览" }));
    const menu = await screen.findByRole("menu", { name: "打开预览" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent))
      .toEqual(["Mework1", "api2", "打开预览面板"]);

    await user.click(within(menu).getByRole("menuitem", { name: /^api/ }));

    await waitFor(() => expect(browserMocks.setBrowserPageNetwork)
      .toHaveBeenCalledWith(conversationId, { conversationId, workspace: 2 }));
    await waitFor(() => expect(browserMocks.openBrowser).toHaveBeenCalled());
    expect(browserMocks.openBrowser.mock.calls[0]?.[0]).toBe(conversationId);
    // The page reads the remote workspace's launch.json, by its number.
    await waitFor(() => expect(previewApi.listPreviewConfigurations)
      .toHaveBeenCalledWith({ conversationId, workspace: 2 }));
    const tabs = await screen.findAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["2api"]);
  });

  it("adds a page from the pane's + for another workspace, as a tab of its own", async () => {
    const document = documentWithRemoteWorkspace();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversationId = document.workspaces[0].conversations[0].id;
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(within(toolbar()).getByRole("button", { name: "预览" }));
    await user.click(within(await screen.findByRole("menu", { name: "打开预览" }))
      .getByRole("menuitem", { name: /^api/ }));
    await screen.findAllByRole("tab");

    await user.click(screen.getByRole("button", { name: "新建预览页面" }));
    const menu = await screen.findByRole("menu", { name: "为哪个工作区打开预览" });
    // The same workspaces, and no pane row: the pane is already open.
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent))
      .toEqual(["Mework1", "api2"]);
    await user.click(within(menu).getByRole("menuitem", { name: /^Mework/ }));

    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2));
    const second = browserMocks.setBrowserPageNetwork.mock.calls.at(-1);
    expect(second?.[0]).toMatch(new RegExp(`^${conversationId}#tab_`));
    expect(second?.[1]).toEqual({ conversationId });
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["2api", "1Mework"]);
  });

  it("files a page under the workspace whose machine the host moved it onto", async () => {
    const document = documentWithRemoteWorkspace();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(within(toolbar()).getByRole("button", { name: "预览" }));
    await user.click(within(await screen.findByRole("menu", { name: "打开预览" }))
      .getByRole("menuitem", { name: /^Mework/ }));
    expect((await screen.findAllByRole("tab")).map((tab) => tab.textContent)).toEqual(["1Mework"]);

    // The model's preview_start ran a server in workspace 2 and put the page on devbox's network.
    browserMocks.getBrowserStatus.mockResolvedValue({
      hasPage: true,
      open: true,
      loading: false,
      url: "http://localhost:5173/",
      title: "Vite App",
      canGoBack: false,
      canGoForward: false,
      zoom: 1,
      viewport: { width: 560, height: 720 },
      networkMachine: "ssh:machine-devbox"
    });

    await waitFor(() => expect(screen.getAllByRole("tab").map((tab) => tab.textContent))
      .toEqual(["2Vite App"]), { timeout: 3000 });
  });

  it("opens the chip-selected workspace's start page when there is no page yet", async () => {
    const document = documentWithRemoteWorkspace();
    runtimeMocks.loadDocument.mockResolvedValue(document);
    const conversationId = document.workspaces[0].conversations[0].id;
    const user = userEvent.setup();
    render(<App />);
    await screen.findByLabelText("向 Agent 发送消息");
    await user.click(screen.getByRole("button", { name: /^工作区：/ }));
    await user.click(within(await screen.findByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitemradio", { name: /^api/ }));

    await user.click(within(toolbar()).getByRole("button", { name: "预览" }));
    await user.click(within(await screen.findByRole("menu", { name: "打开预览" }))
      .getByRole("menuitem", { name: "打开预览面板" }));

    await waitFor(() => expect(browserMocks.setBrowserPageNetwork)
      .toHaveBeenCalledWith(conversationId, { conversationId, workspace: 2 }));
    expect((await screen.findAllByRole("tab")).map((tab) => tab.textContent)).toEqual(["2api"]);
  });
});
