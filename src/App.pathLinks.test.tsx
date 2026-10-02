import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { configureI18n } from "./i18n";
import type { BrowseProbeResult, BrowseProbeTarget } from "./lib/fileBrowser";
import { installPathLinkInterceptor } from "./lib/pathLinks";
import { documentWithModel, resetAppMocks, runtimeMocks } from "./test/appMocks";

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

const browse = vi.hoisted(() => ({
  probe: vi.fn<(targets: readonly BrowseProbeTarget[]) => Promise<BrowseProbeResult[]>>(),
  list: vi.fn(),
  read: vi.fn()
}));
vi.mock("./lib/fileBrowser", async (importOriginal) => ({
  ...await importOriginal<typeof import("./lib/fileBrowser")>(),
  browseProbePaths: browse.probe,
  browseListDirectory: browse.list,
  browseReadFile: browse.read
}));

const ubuntu = { kind: "ssh" as const, machineId: "ubuntu-id" };
const LOCAL_ROOT = "C:/test/Mework";

/** The conversation works in its project's directory here and in `/srv/app` on an SSH machine. */
function twoMachineDocument() {
  const document = documentWithModel();
  document.globalSettings.executionEnvironments.sshMachines = [{
    id: "ubuntu-id",
    name: "ubuntu",
    host: "dev@100.88.12.34",
    port: 0,
    identityFile: "",
    createdAt: "",
    updatedAt: ""
  }];
  const conversation = document.workspaces[0]!.conversations[0]!;
  conversation.attachedWorkspaces = [{ machine: ubuntu, path: "/srv/app" }];
  conversation.contexts = [
    { id: "ask", kind: "user", content: "在哪", createdAt: "2026-10-01T00:00:00Z" },
    { id: "answer", kind: "assistant", content: "改在 `src/main.rs` 里。", createdAt: "2026-10-01T00:00:01Z" }
  ];
  return document;
}

/** Each target found or not, by machine and path. */
function answering(found: (target: BrowseProbeTarget) => boolean) {
  browse.probe.mockImplementation(async (targets) => targets.map((target) => ({
    path: target.path,
    kind: found(target) ? "file" : null,
    reached: true
  })));
}

let uninstall: () => void = () => {};

beforeEach(() => {
  resetAppMocks();
  browse.probe.mockReset();
  browse.list.mockReset().mockImplementation(async (_machine: unknown, path: string) => ({
    path,
    parent: null,
    windows: false,
    entries: [],
    truncated: false
  }));
  browse.read.mockReset().mockImplementation(async (_machine: unknown, path: string) => ({
    path,
    content: "fn main() {}\n",
    truncated: false,
    size: 13,
    binary: false
  }));
  runtimeMocks.loadDocument.mockResolvedValue(twoMachineDocument());
  uninstall = installPathLinkInterceptor(window.document, async () => undefined);
});

afterEach(() => {
  uninstall();
  configureI18n("zh-CN");
});

async function pathLink() {
  return screen.findByRole("button", { name: "src/main.rs" });
}

describe("App — a path the transcript names", () => {
  it("asks which workspace when the path is in more than one, at the link", async () => {
    answering(() => true);
    const user = userEvent.setup();
    render(<App />);

    await user.click(await pathLink());

    const menu = await screen.findByRole("menu", { name: "打开位置" });
    expect(within(menu).getByText("在哪个工作区打开")).toBeInTheDocument();
    const choices = within(menu).getAllByRole("menuitem");
    expect(choices.map((choice) => choice.textContent)).toEqual([
      `Mework${LOCAL_ROOT}/src/main.rs1`,
      "appdev@100.88.12.34:/srv/app/src/main.rs2"
    ]);
    expect(browse.probe).toHaveBeenCalledWith([
      { machine: null, path: `${LOCAL_ROOT}/src/main.rs` },
      { machine: ubuntu, path: "/srv/app/src/main.rs" }
    ]);

    await user.click(choices[1]!);
    expect(await screen.findByRole("region", { name: "文件" })).toBeInTheDocument();
    await waitFor(() => expect(browse.read).toHaveBeenCalledWith(ubuntu, "/srv/app/src/main.rs"));
    expect(screen.queryByRole("menu", { name: "打开位置" })).not.toBeInTheDocument();
  });

  it("opens the one workspace that has the path without asking", async () => {
    answering((target) => target.machine === null);
    const user = userEvent.setup();
    render(<App />);

    await user.click(await pathLink());

    expect(await screen.findByRole("region", { name: "文件" })).toBeInTheDocument();
    await waitFor(() => expect(browse.read).toHaveBeenCalledWith(null, `${LOCAL_ROOT}/src/main.rs`));
    expect(screen.queryByRole("menu", { name: "打开位置" })).not.toBeInTheDocument();
  });

  /** The pointer reaching the link starts the lookup; the click finds it already on its way. */
  it("looks the path up once, when the pointer reaches it", async () => {
    answering((target) => target.machine !== null);
    const user = userEvent.setup();
    render(<App />);
    const link = await pathLink();

    fireEvent.mouseOver(link);
    await waitFor(() => expect(browse.probe).toHaveBeenCalledTimes(1));
    await user.click(link);

    await waitFor(() => expect(browse.read).toHaveBeenCalledWith(ubuntu, "/srv/app/src/main.rs"));
    expect(browse.probe).toHaveBeenCalledTimes(1);
  });

  /** A lookup that is slow shows the menu at once, saying it is still looking. */
  it("opens the menu while a slow lookup is still out", async () => {
    let answer: (results: BrowseProbeResult[]) => void = () => {};
    browse.probe.mockImplementation(() => new Promise((resolve) => { answer = resolve; }));
    const user = userEvent.setup();
    render(<App />);

    await user.click(await pathLink());

    const menu = await screen.findByRole("menu", { name: "打开位置" });
    expect(menu).toHaveTextContent("正在查找…");
    answer([
      { path: `${LOCAL_ROOT}/src/main.rs`, kind: "file", reached: true },
      { path: "/srv/app/src/main.rs", kind: null, reached: true }
    ]);

    await waitFor(() => expect(screen.queryByRole("menu", { name: "打开位置" })).not.toBeInTheDocument());
    await waitFor(() => expect(browse.read).toHaveBeenCalledWith(null, `${LOCAL_ROOT}/src/main.rs`));
  });

  /** A path found nowhere opens where the pane can say so, not on a machine that is switched off. */
  it("opens a path found nowhere on a machine that answered", async () => {
    const document = twoMachineDocument();
    const project = document.workspaces[0]!;
    project.conversations[0]!.attachedWorkspaces = [{ machine: null, path: project.path }];
    project.machine = ubuntu;
    project.path = "/srv/app";
    runtimeMocks.loadDocument.mockResolvedValue(document);
    browse.probe.mockImplementation(async (targets) => targets.map((target) => ({
      path: target.path,
      kind: null,
      reached: target.machine === null
    })));
    const user = userEvent.setup();
    render(<App />);

    await user.click(await pathLink());

    await waitFor(() => expect(browse.read).toHaveBeenCalledWith(null, `${LOCAL_ROOT}/src/main.rs`));
    expect(browse.read).not.toHaveBeenCalledWith(ubuntu, expect.anything());
  });
});
