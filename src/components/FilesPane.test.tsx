import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BrowseEntry,
  BrowseEntryKind,
  BrowseFileBytes,
  BrowseMachine,
  BrowseSearchResults,
  BrowseTextFile
} from "../lib/fileBrowser";
import type { SshMachineConfig } from "../types";
import { FilesPane } from "./FilesPane";
import type { FilesPaneProps } from "./FilesPane";

const backend = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => true),
  invoke: vi.fn()
}));

vi.mock("../lib/backend", () => backend);

const ROOT = "/w/mework";
const ubuntu: SshMachineConfig = {
  id: "ubuntu-id",
  name: "ubuntu",
  host: "dev@100.88.12.34",
  port: 0,
  identityFile: "",
  createdAt: "",
  updatedAt: ""
};
const ubuntuMachine: BrowseMachine = { kind: "ssh", machineId: "ubuntu-id" };

/** Every machine's files, by machine key and then absolute path. */
let directories: Record<string, Record<string, BrowseEntry[] | Error>>;
let files: Record<string, Record<string, BrowseTextFile | Error>>;
let pictures: Record<string, Record<string, BrowseFileBytes | Error>>;
let searchResults: BrowseSearchResults | Error | null;
/** Where `~` leads on each machine. */
let homes: Record<string, string>;

function key(machine: BrowseMachine | undefined): string {
  if (!machine) return "local";
  return machine.kind === "wsl" ? `wsl:${machine.distro}` : `ssh:${machine.machineId}`;
}

function entry(parent: string, name: string, kind: BrowseEntryKind = "file"): BrowseEntry {
  return { name, path: `${parent === "/" ? "" : parent}/${name}`, kind, link: false, size: kind === "file" ? 12 : null };
}

function textFile(path: string, content: string): BrowseTextFile {
  return { path, content, truncated: false, size: content.length, binary: false };
}

function binaryFile(path: string): BrowseTextFile {
  return { path, content: "", truncated: false, size: 4096, binary: true };
}

/** A one-pixel PNG is enough: the viewer only ever hands the bytes to an `<img>`. */
const ONE_PIXEL_PNG = "iVBORw0KGgoAAAANSUhEUg==";

function pictureFile(path: string, data = ONE_PIXEL_PNG): BrowseFileBytes {
  return { path, data, size: 4096, tooLarge: false };
}

function calls(command: string): Record<string, unknown>[] {
  return backend.invoke.mock.calls
    .filter(([name]) => name === command)
    .map(([, args]) => args as Record<string, unknown>);
}

function listingCalls(): string[] {
  return calls("browse_list_directory").map((args) => args.path as string);
}

function searchCalls(): string[] {
  return calls("browse_search_files").map((args) => args.query as string);
}

/**
 * Every tree row carries a hidden `⋮` button, so a row's accessible name is its
 * label plus the button's own label; these anchors match the name's head.
 */
function rowName(name: string): RegExp {
  return new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
}

/**
 * Names are user data, so the fixtures are looked up by own key: a workspace
 * holding `constructor` must not answer with something the prototype carries.
 */
function fixture<T>(table: Record<string, T> | undefined, name: string): T | undefined {
  return table && Object.hasOwn(table, name) ? table[name] : undefined;
}

function expand(machine: BrowseMachine, path: string): string {
  if (path === "~" || path === "") return homes[key(machine)] ?? "/";
  if (path.startsWith("~/")) return `${homes[key(machine)]}/${path.slice(2)}`;
  return path.length > 1 ? path.replace(/\/+$/, "") : path;
}

function respond(command: string, args: Record<string, unknown>): unknown {
  const machine = (args.machine ?? null) as BrowseMachine;
  const path = typeof args.path === "string" ? expand(machine, args.path) : "";
  if (command === "browse_list_directory") {
    const entries = fixture(directories[key(machine)], path);
    if (entries === undefined) throw new Error(`未知目录：${path}`);
    if (entries instanceof Error) throw entries;
    return { path, parent: null, windows: false, entries, truncated: false };
  }
  if (command === "browse_read_file") {
    const file = fixture(files[key(machine)], path);
    if (file === undefined) throw new Error(`未知文件：${path}`);
    if (file instanceof Error) throw file;
    return file;
  }
  if (command === "browse_read_file_bytes") {
    const picture = fixture(pictures[key(machine)], path);
    if (picture === undefined) throw new Error(`未知文件：${path}`);
    if (picture instanceof Error) throw picture;
    return picture;
  }
  if (command === "browse_search_files") {
    if (searchResults === null) throw new Error("未配置的搜索结果");
    if (searchResults instanceof Error) throw searchResults;
    return searchResults;
  }
  if (command === "browse_probe_paths") {
    return (args.targets as { machine: BrowseMachine; path: string }[]).map((target) => {
      const resolved = expand(target.machine, target.path);
      const kind = fixture(directories[key(target.machine)], resolved) !== undefined
        ? "directory"
        : fixture(files[key(target.machine)], resolved) !== undefined || fixture(pictures[key(target.machine)], resolved) !== undefined
          ? "file"
          : null;
      return { path: resolved, kind, reached: true };
    });
  }
  if (command === "browse_rename_path") return { path: `${(args.path as string).replace(/\/[^/]*$/, "")}/${args.name as string}` };
  if (command === "browse_create_directory") return { path: `${args.parent as string}/${args.name as string}` };
  if (command === "browse_delete_path") return { trashed: !args.machine };
  if (command === "browse_open_in_file_manager" || command === "browse_open_with") return undefined;
  if (command === "browse_open_with_choices") {
    return {
      apps: [
        { id: "/Applications/TextEdit.app", name: "TextEdit", icon: null, default: true },
        { id: "/Applications/Code.app", name: "Visual Studio Code", icon: null, default: false }
      ],
      chooser: false
    };
  }
  throw new Error(`未预期的命令：${command}`);
}

interface DeferredRead {
  resolve: (file: BrowseTextFile) => void;
  reject: (error: Error) => void;
}

/** Listings answer at once; every file read is held so the test settles them in its own order. */
function holdFileReads(): DeferredRead[] {
  const reads: DeferredRead[] = [];
  backend.invoke.mockImplementation((command: string, args: Record<string, unknown>) => {
    if (command !== "browse_read_file") {
      try {
        return Promise.resolve(respond(command, args));
      } catch (error) {
        return Promise.reject(error);
      }
    }
    return new Promise<BrowseTextFile>((resolve, reject) => { reads.push({ resolve, reject }); });
  });
  return reads;
}

function fullProps(overrides: Partial<FilesPaneProps> = {}): FilesPaneProps {
  return {
    paneId: "files",
    workspaces: [{ number: 1, machine: null, path: ROOT, label: "mework" }],
    sshMachines: [ubuntu],
    hostWindows: false,
    active: true,
    expanded: false,
    onToggleExpand: vi.fn(),
    onPaneFocus: vi.fn(),
    onPaneClose: vi.fn(),
    ...overrides
  };
}

function renderPane(overrides: Partial<FilesPaneProps> = {}) {
  return render(<FilesPane {...fullProps(overrides)} />);
}

beforeEach(() => {
  window.localStorage.clear();
  backend.hasBackendRuntime.mockReturnValue(true);
  backend.invoke.mockReset();
  homes = { local: "/home/me", "ssh:ubuntu-id": "/home/dev" };
  directories = {
    local: {
      [ROOT]: [entry(ROOT, "Docs", "directory"), entry(ROOT, "src", "directory"), entry(ROOT, "readme.txt")],
      [`${ROOT}/src`]: [entry(`${ROOT}/src`, "lib", "directory"), entry(`${ROOT}/src`, "App.tsx")],
      [`${ROOT}/src/lib`]: [],
      [`${ROOT}/Docs`]: [entry(`${ROOT}/Docs`, "guide.md")],
      "/w": [entry("/w", "mework", "directory")],
      "/": [entry("/", "w", "directory")]
    },
    "ssh:ubuntu-id": {
      "/home/dev": [entry("/home/dev", "notes.txt")],
      "/srv": [entry("/srv", "app", "directory")]
    }
  };
  files = {
    local: {
      [`${ROOT}/readme.txt`]: textFile(`${ROOT}/readme.txt`, "# Mework\n第二行\n"),
      [`${ROOT}/src/App.tsx`]: textFile(`${ROOT}/src/App.tsx`, "export {};\n"),
      [`${ROOT}/Docs/guide.md`]: textFile(`${ROOT}/Docs/guide.md`, "指南\n")
    },
    "ssh:ubuntu-id": {
      "/home/dev/notes.txt": textFile("/home/dev/notes.txt", "远端笔记\n")
    }
  };
  pictures = { local: {}, "ssh:ubuntu-id": {} };
  searchResults = null;
  backend.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => respond(command, args));
});

describe("FilesPane", () => {
  /** The host sorts; the pane must not undo that order on its way to the screen. */
  it("lists the workspace root with directories first", async () => {
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    const rows = screen.getAllByRole("treeitem");
    expect(rows.map((row) => row.textContent)).toEqual(["Docs", "src", "readme.txt"]);
    expect(listingCalls()).toEqual([ROOT]);
  });

  it("names the page's place in the title bar and the address bar", async () => {
    const { container } = renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    const paneTitle = container.querySelector(".files-pane__pane-title");
    expect(paneTitle).toHaveTextContent("文件");
    expect(paneTitle).toHaveAttribute("title", ROOT);
    expect(screen.getByRole("button", { name: `位置：${ROOT}，点按编辑` })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /mework/ })).toBeInTheDocument();
  });

  it("lazy-loads a directory on expansion and keeps the listing", async () => {
    const user = userEvent.setup();
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("App.tsx") });
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);
    expect(screen.getByRole("treeitem", { name: rowName("src") })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("treeitem", { name: rowName("App.tsx") })).toHaveAttribute("aria-level", "2");

    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await waitFor(() => expect(screen.queryByRole("treeitem", { name: rowName("App.tsx") })).not.toBeInTheDocument());
    // Clicked twice within the double click's window would go into the folder.
    await new Promise((resolve) => { setTimeout(resolve, 250); });
    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("App.tsx") });
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);
  });

  /** Directory names are user data: a folder called `constructor` is a folder, not a prototype member. */
  it("expands a directory whose name is also an Object.prototype member", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "constructor", "directory"), entry(ROOT, "readme.txt")];
    directories.local[`${ROOT}/constructor`] = [entry(`${ROOT}/constructor`, "valueOf.ts")];
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("constructor") }));

    expect(await screen.findByRole("treeitem", { name: rowName("valueOf.ts") })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: rowName("constructor") })).toHaveAttribute("aria-expanded", "true");
  });

  it("opens a file in a line-numbered viewer on the page", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    const code = await screen.findByLabelText("readme.txt");
    expect(code).toHaveClass("files-pane__code");
    expect(Array.from(container.querySelectorAll(".numbered-code__number"), (node) => node.textContent)).toEqual(["1", "2"]);
    expect(Array.from(container.querySelectorAll(".numbered-code__text"), (node) => node.textContent)).toEqual(["# Mework", "第二行"]);
    // The page takes the file's name; it is still the one page.
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    expect(screen.getByRole("tab", { name: /readme\.txt/ })).toBeInTheDocument();
  });

  it("keeps the tree beside the viewer once a file is open", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");

    expect(screen.getByRole("tree")).toBeInTheDocument();
    expect(container.querySelector("[data-files-tree]")).not.toHaveAttribute("hidden");
    // Opening a file does not re-read the tree; the listing the user built stays.
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);
  });

  it("says a binary file cannot be shown instead of rendering it", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/readme.txt`] = binaryFile(`${ROOT}/readme.txt`);
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByText("二进制文件，无法显示")).toBeInTheDocument();
    expect(container.querySelector(".files-pane__code")).toBeNull();
  });

  it("warns that a truncated file is missing its tail", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/readme.txt`] = { path: `${ROOT}/readme.txt`, content: "开头", truncated: true, size: 2_000_000, binary: false };
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByText("文件过大，只显示了开头部分")).toBeInTheDocument();
    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("开头");
  });

  /** A failed child keeps the rest of the tree: only that directory reports the failure. */
  it("puts a rejected child listing in its own row", async () => {
    const user = userEvent.setup();
    directories.local[`${ROOT}/src`] = new Error("没有权限访问 src");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));

    expect(await screen.findByText("没有权限访问 src")).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: rowName("readme.txt") })).toBeInTheDocument();
  });

  it("replaces the tree when the root itself cannot be read", async () => {
    directories.local[ROOT] = new Error("无法读取这个文件夹");
    renderPane();

    expect(await screen.findByRole("alert")).toHaveTextContent("无法读取这个文件夹");
    expect(screen.queryByRole("tree")).not.toBeInTheDocument();
  });

  it("shows the host's message when a file cannot be read", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/readme.txt`] = new Error("这个文件已经不在了");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByRole("alert")).toHaveTextContent("这个文件已经不在了");
  });

  /** Closing and reopening leaves two reads in flight; the older one is no longer the answer. */
  it("keeps the newer read when an earlier read of the same file lands late", async () => {
    const user = userEvent.setup();
    const reads = holdFileReads();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await user.click(await screen.findByRole("button", { name: "关闭文件" }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await waitFor(() => expect(reads).toHaveLength(2));

    await act(async () => { reads[1]!.resolve(textFile(`${ROOT}/readme.txt`, "现在的内容\n")); });
    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("现在的内容");

    await act(async () => { reads[0]!.resolve(textFile(`${ROOT}/readme.txt`, "过时的内容\n")); });

    expect(screen.getByLabelText("readme.txt")).toHaveTextContent("现在的内容");
    expect(screen.getByLabelText("readme.txt")).not.toHaveTextContent("过时的内容");
  });

  it("keeps the newer read when an earlier read of the same file fails late", async () => {
    const user = userEvent.setup();
    const reads = holdFileReads();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await user.click(await screen.findByRole("button", { name: "关闭文件" }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await waitFor(() => expect(reads).toHaveLength(2));

    await act(async () => { reads[1]!.resolve(textFile(`${ROOT}/readme.txt`, "现在的内容\n")); });
    await screen.findByLabelText("readme.txt");
    await act(async () => { reads[0]!.reject(new Error("这个文件已经不在了")); });

    expect(screen.getByLabelText("readme.txt")).toHaveTextContent("现在的内容");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("re-reads every open listing and the open file from the pane menu's refresh", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);

    files.local[`${ROOT}/src/App.tsx`] = textFile(`${ROOT}/src/App.tsx`, "export const refreshed = true;\n");
    await user.click(screen.getByRole("button", { name: "文件 设置" }));
    await user.click(await screen.findByRole("menuitem", { name: "刷新" }));

    await waitFor(() => expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`, ROOT, `${ROOT}/src`]));
    expect(await screen.findByLabelText("src/App.tsx")).toHaveTextContent("export const refreshed = true;");
  });

  /** A refresh abandons the answers in flight; the directories waiting for them must not stay stuck. */
  it("re-requests a directory whose pending listing the refresh threw away", async () => {
    const user = userEvent.setup();
    let releaseFirst: () => void = () => {};
    let sourceRequests = 0;
    backend.invoke.mockImplementation((command: string, args: Record<string, unknown>) => {
      if (command === "browse_list_directory" && args.path === `${ROOT}/src`) {
        sourceRequests += 1;
        const answer = respond(command, args);
        if (sourceRequests > 1) return Promise.resolve(answer);
        return new Promise((resolve) => { releaseFirst = () => resolve(answer); });
      }
      try {
        return Promise.resolve(respond(command, args));
      } catch (error) {
        return Promise.reject(error);
      }
    });
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    expect(sourceRequests).toBe(1);
    // Past the double click's window and the wait for its listing, the folder opens on a reading row.
    expect(await screen.findByText("正在读取…")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "文件 设置" }));
    await user.click(await screen.findByRole("menuitem", { name: "刷新" }));
    await act(async () => { releaseFirst(); });

    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(sourceRequests).toBe(2);
  });

  it("goes into a folder on a double click", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.dblClick(await screen.findByRole("treeitem", { name: rowName("src") }));

    expect(await screen.findByRole("button", { name: `位置：${ROOT}/src，点按编辑` })).toBeInTheDocument();
    expect(screen.getAllByRole("treeitem").map((row) => row.textContent)).toEqual(["lib", "App.tsx"]);
    // The first click of the two did not open the folder once its window ran out.
    await new Promise((resolve) => { setTimeout(resolve, 250); });
    expect(screen.getAllByRole("treeitem").map((row) => row.textContent)).toEqual(["lib", "App.tsx"]);
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);
  });

  it("opens a folder on a single click only once a double click's window has passed", async () => {
    const user = userEvent.setup();
    renderPane();

    const source = await screen.findByRole("treeitem", { name: rowName("src") });
    await user.click(source);
    // Read at the press, before the folder opens.
    expect(listingCalls()).toEqual([ROOT, `${ROOT}/src`]);
    expect(source).toHaveAttribute("aria-expanded", "false");

    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(source).toHaveAttribute("aria-expanded", "true");
    expect(screen.queryByText("正在读取…")).not.toBeInTheDocument();
  });

  it("holds a folder shut while its listing is still being read, then opens it whole", async () => {
    const user = userEvent.setup();
    let release: () => void = () => {};
    backend.invoke.mockImplementation((command: string, args: Record<string, unknown>) => {
      const answer = (() => {
        try {
          return Promise.resolve(respond(command, args));
        } catch (error) {
          return Promise.reject(error);
        }
      })();
      if (command === "browse_list_directory" && args.path === `${ROOT}/src`) {
        return new Promise((resolve) => { release = () => resolve(answer); });
      }
      return answer;
    });
    renderPane();

    const source = await screen.findByRole("treeitem", { name: rowName("src") });
    await user.click(source);
    await new Promise((resolve) => { setTimeout(resolve, 250); });
    expect(source).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("正在读取…")).not.toBeInTheDocument();

    await act(async () => { release(); });
    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(source).toHaveAttribute("aria-expanded", "true");
  });

  it("scrolls a revealed row into the tree once, and leaves the tree where it is scrolled after", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();
    await user.dblClick(await screen.findByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("App.tsx") });

    // jsdom lays nothing out: the tree is a 100px box over a taller list, every row 200px down it.
    const tree = container.querySelector<HTMLElement>(".files-pane__tree")!;
    tree.style.overflowY = "auto";
    Object.defineProperty(tree, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(tree, "clientHeight", { configurable: true, value: 100 });
    const scrolls: number[] = [];
    let scrollTop = 0;
    Object.defineProperty(tree, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => { scrollTop = value; scrolls.push(value); }
    });
    const box = (top: number, height: number) => ({
      x: 0, y: top, left: 0, top, width: 200, height, right: 200, bottom: top + height, toJSON: () => ({})
    }) as DOMRect;
    const measure = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this === tree) return box(0, 100);
      return this.getAttribute("role") === "treeitem" ? box(200, 24) : box(0, 0);
    });
    const pageScroll = vi.spyOn(HTMLElement.prototype, "scrollIntoView");
    try {
      // Up a level: the folder just left is revealed in its parent.
      act(() => screen.getAllByRole("treeitem")[0]!.focus());
      await user.keyboard("{Backspace}");
      await screen.findByRole("treeitem", { name: rowName("Docs") });
      await waitFor(() => expect(scrolls).toEqual([124]));

      // The reader scrolls away; a folder read ahead under the pointer changes the rows.
      scrollTop = 0;
      await user.hover(screen.getByRole("treeitem", { name: rowName("Docs") }));
      await waitFor(() => expect(listingCalls()).toContain(`${ROOT}/Docs`));
      expect(scrollTop).toBe(0);
      expect(scrolls).toEqual([124]);
      expect(pageScroll).not.toHaveBeenCalled();
    } finally {
      measure.mockRestore();
      pageScroll.mockRestore();
    }
  });

  it("reads a folder ahead while the pointer rests on it", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.hover(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await waitFor(() => expect(listingCalls()).toEqual([ROOT, `${ROOT}/Docs`]));
    expect(screen.getByRole("treeitem", { name: rowName("Docs") })).toHaveAttribute("aria-expanded", "false");
  });

  it("searches the page's directory from the filter and opens a match", async () => {
    const user = userEvent.setup();
    searchResults = {
      query: "app",
      root: ROOT,
      matches: [{ name: "App.tsx", path: "src/App.tsx", kind: "file", positions: [4, 5, 6], score: 1 }],
      truncated: false
    };
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.type(screen.getByRole("textbox", { name: "筛选文件" }), " app");

    const listbox = await screen.findByRole("listbox", { name: "匹配的文件" });
    const option = await within(listbox).findByRole("option", { name: /App\.tsx/ });
    // The debounce settles once, with the trimmed query.
    await waitFor(() => expect(searchCalls()).toEqual(["app"]));
    expect(backend.invoke).toHaveBeenCalledWith(
      "browse_search_files",
      { machine: null, root: ROOT, query: "app", limit: 200 }
    );

    await user.click(option);
    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "清除筛选" }));
    expect(await screen.findByRole("tree")).toBeInTheDocument();
    expect(searchCalls()).toEqual(["app"]);
  });

  it("does not search while the filter is empty", async () => {
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    // Outlive the filter's debounce; an empty query must never reach the host.
    await act(async () => { await new Promise((resolve) => { setTimeout(resolve, 200); }); });

    expect(searchCalls()).toEqual([]);
  });

  it("walks the tree with the arrow keys and opens with Enter", async () => {
    const user = userEvent.setup();
    renderPane();

    const source = await screen.findByRole("treeitem", { name: rowName("src") });
    act(() => source.focus());
    await user.keyboard("{ArrowRight}");

    const nested = await screen.findByRole("treeitem", { name: rowName("lib") });
    expect(source).toHaveAttribute("aria-expanded", "true");
    expect(document.activeElement).toBe(source);

    await user.keyboard("{ArrowRight}");
    expect(document.activeElement).toBe(nested);

    await user.keyboard("{ArrowDown}");
    const app = screen.getByRole("treeitem", { name: rowName("App.tsx") });
    expect(document.activeElement).toBe(app);

    await user.keyboard("{Enter}");
    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
  });

  it("collapses with ArrowLeft and climbs to the parent from a child", async () => {
    const user = userEvent.setup();
    renderPane();

    const source = await screen.findByRole("treeitem", { name: rowName("src") });
    act(() => source.focus());
    await user.keyboard("{ArrowRight}");
    const app = await screen.findByRole("treeitem", { name: rowName("App.tsx") });

    act(() => app.focus());
    await user.keyboard("{ArrowLeft}");
    expect(document.activeElement).toBe(screen.getByRole("treeitem", { name: rowName("src") }));

    await user.keyboard("{ArrowLeft}");
    expect(screen.getByRole("treeitem", { name: rowName("src") })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("treeitem", { name: rowName("App.tsx") })).not.toBeInTheDocument();
  });

  /** Only the focused row is in the tab order; the rest are reached with the arrows. */
  it("keeps a single tab stop on the tree", async () => {
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    const rows = screen.getAllByRole("treeitem");
    expect(rows.map((row) => row.getAttribute("tabindex"))).toEqual(["0", "-1", "-1"]);

    act(() => rows[2]!.focus());
    await waitFor(() => {
      expect(screen.getAllByRole("treeitem").map((row) => row.getAttribute("tabindex"))).toEqual(["-1", "-1", "0"]);
    });
  });

  it("waits for the pane to be shown before touching the host", async () => {
    const { rerender } = renderPane({ active: false });

    expect(backend.invoke).not.toHaveBeenCalled();

    rerender(<FilesPane {...fullProps()} />);

    expect(await screen.findByRole("treeitem", { name: rowName("src") })).toBeInTheDocument();
  });

  it("shows a loading row until the root answers", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => { release = resolve; });
    backend.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
      if (command === "browse_list_directory") await gate;
      return respond(command, args);
    });
    renderPane();

    expect(screen.getByText("正在读取…")).toBeInTheDocument();
    release();

    expect(await screen.findByRole("treeitem", { name: rowName("src") })).toBeInTheDocument();
  });

  it("says so when a folder holds nothing", async () => {
    directories.local[`${ROOT}/Docs`] = [];
    const user = userEvent.setup();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));

    expect(await within(screen.getByRole("tree")).findByText("文件夹为空")).toBeInTheDocument();
  });

  /** A workspace recorded as `~/…` on its machine is shown at the path that machine gives it. */
  it("opens a workspace recorded under ~ where its machine says it is", async () => {
    directories["ssh:ubuntu-id"]!["/home/dev/app"] = [entry("/home/dev/app", "main.py")];
    renderPane({ workspaces: [{ number: 1, machine: ubuntuMachine, path: "~/app", label: "app" }] });

    expect(await screen.findByRole("treeitem", { name: rowName("main.py") })).toBeInTheDocument();
    expect(calls("browse_list_directory")[0]).toEqual({ machine: ubuntuMachine, path: "~/app" });
    expect(await screen.findByRole("button", { name: "位置：dev@100.88.12.34:/home/dev/app，点按编辑" })).toBeInTheDocument();
  });
});

/** The address bar: where the page is, with its machine, and a field for going somewhere else. */
describe("FilesPane address bar", () => {
  it("goes up to the top of the machine and from there to the machines", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: "上一级" }));
    expect(await screen.findByRole("button", { name: "位置：/w，点按编辑" })).toBeInTheDocument();
    // The folder just left is the one lit.
    expect(await screen.findByRole("treeitem", { name: rowName("mework") })).toHaveClass("files-pane__row--revealed");

    await user.click(screen.getByRole("button", { name: "上一级" }));
    expect(await screen.findByRole("button", { name: "位置：/，点按编辑" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "上一级" }));

    expect(await screen.findByRole("tree", { name: "机器" })).toBeInTheDocument();
    expect(screen.getAllByRole("treeitem").map((row) => row.textContent)).toEqual(["本机", "ubuntudev@100.88.12.34"]);
    expect(screen.getByRole("button", { name: "上一级" })).toBeDisabled();
  });

  it("enters a machine from the machines at its home", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });
    await user.click(screen.getByRole("button", { name: `位置：${ROOT}，点按编辑` }));
    const field = screen.getByRole("textbox", { name: "位置" });
    await user.clear(field);
    await user.type(field, "{Enter}");
    await user.click(await screen.findByRole("treeitem", { name: /^ubuntu/ }));

    expect(await screen.findByRole("treeitem", { name: rowName("notes.txt") })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "位置：dev@100.88.12.34:/home/dev，点按编辑" })).toBeInTheDocument();
  });

  it("takes a typed SSH address to that machine's directory", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: `位置：${ROOT}，点按编辑` }));
    const field = screen.getByRole("textbox", { name: "位置" });
    expect(field).toHaveValue(ROOT);
    await user.clear(field);
    await user.type(field, "ubuntu:/srv{Enter}");

    expect(await screen.findByRole("treeitem", { name: rowName("app") })).toBeInTheDocument();
    expect(calls("browse_probe_paths")).toContainEqual({ targets: [{ machine: ubuntuMachine, path: "/srv" }], patient: true });
    expect(calls("browse_list_directory")).toContainEqual({ machine: ubuntuMachine, path: "/srv" });
  });

  it("opens a typed file beside its folder", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: `位置：${ROOT}，点按编辑` }));
    const field = screen.getByRole("textbox", { name: "位置" });
    await user.clear(field);
    await user.type(field, "dev@100.88.12.34:~/notes.txt{Enter}");

    expect(await screen.findByLabelText("home/dev/notes.txt")).toHaveTextContent("远端笔记");
    expect(await screen.findByRole("treeitem", { name: rowName("notes.txt") })).toHaveAttribute("aria-current", "true");
  });

  it("says when a typed machine is not registered, and keeps what was typed", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: `位置：${ROOT}，点按编辑` }));
    const field = screen.getByRole("textbox", { name: "位置" });
    await user.clear(field);
    await user.type(field, "nowhere:/srv{Enter}");

    expect(await screen.findByRole("alert")).toHaveTextContent("没有登记名为 nowhere 的 SSH 机器");
    expect(field).toHaveValue("nowhere:/srv");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "位置" })).not.toBeInTheDocument();
  });
});

/** The pane as a file manager: menus on every row and on the space between them. */
describe("FilesPane menus", () => {
  async function rowMenu(name: string) {
    const row = await screen.findByRole("treeitem", { name: rowName(name) });
    fireEvent.contextMenu(row, { clientX: 40, clientY: 40 });
    return screen.findByRole("menu", { name: "文件操作" });
  }

  it("offers a file's actions on a right click and on its ⋮", async () => {
    const user = userEvent.setup();
    renderPane();

    const menu = await rowMenu("readme.txt");
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "在文件管理器中打开",
      "打开文件",
      "用…打开",
      "在新页面打开",
      "重命名…",
      "删除…",
      "复制绝对路径",
      "复制相对路径"
    ]);
    await user.click(within(menu).getByRole("menuitem", { name: "复制相对路径" }));
    expect(await navigator.clipboard.readText()).toBe("readme.txt");

    const row = screen.getByRole("treeitem", { name: rowName("readme.txt") });
    await user.click(within(row).getByRole("button", { name: "文件操作" }));
    const again = await screen.findByRole("menu", { name: "文件操作" });
    await user.click(within(again).getByRole("menuitem", { name: "复制绝对路径" }));
    expect(await navigator.clipboard.readText()).toBe(`${ROOT}/readme.txt`);
  });

  it("opens a file or a folder in a page of its own", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "在新页面打开" }));
    expect(await screen.findByLabelText("readme.txt")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);

    await user.click(within(await rowMenu("src")).getByRole("menuitem", { name: "在新页面打开" }));
    expect(await screen.findByRole("button", { name: `位置：${ROOT}/src，点按编辑` })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(3);
  });

  it("lists the programs that open a file when Open With is pointed at", async () => {
    const user = userEvent.setup();
    renderPane();

    const menu = await rowMenu("readme.txt");
    await user.hover(within(menu).getByRole("menuitem", { name: "用…打开" }));
    const program = await screen.findByRole("menuitem", { name: /Visual Studio Code/ });
    expect(screen.getByRole("menuitem", { name: /TextEdit.*默认/ })).toBeInTheDocument();
    expect(calls("browse_open_with_choices")).toEqual([{ path: `${ROOT}/readme.txt` }]);

    await user.click(program);
    expect(calls("browse_open_with")).toEqual([{ path: `${ROOT}/readme.txt`, app: "/Applications/Code.app" }]);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("keeps this computer's desktop out of reach for another machine's files", async () => {
    renderPane({ workspaces: [{ number: 1, machine: ubuntuMachine, path: "/home/dev", label: "dev" }] });

    const menu = await rowMenu("notes.txt");
    expect(within(menu).getByRole("menuitem", { name: "在文件管理器中打开" })).toBeDisabled();
    expect(within(menu).getByRole("menuitem", { name: "用…打开" })).toBeDisabled();
  });

  it("renames a row in place", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "重命名…" }));
    const field = await screen.findByRole("textbox", { name: "新名称" });
    expect(field).toHaveValue("readme.txt");
    await user.clear(field);
    await user.type(field, "README.md{Enter}");

    await waitFor(() => expect(calls("browse_rename_path")).toEqual([{ machine: null, path: `${ROOT}/readme.txt`, name: "README.md" }]));
    await waitFor(() => expect(listingCalls().filter((path) => path === ROOT)).toHaveLength(2));
  });

  it("asks before a delete, saying where it goes", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "删除…" }));
    const confirm = await screen.findByRole("alertdialog", { name: "确认删除" });
    expect(confirm).toHaveTextContent("把“readme.txt”移到废纸篓？");
    expect(calls("browse_delete_path")).toEqual([]);

    await user.click(within(confirm).getByRole("button", { name: "移到废纸篓" }));
    await waitFor(() => expect(calls("browse_delete_path")).toEqual([{ machine: null, path: `${ROOT}/readme.txt` }]));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("warns that a delete on another machine is for good", async () => {
    const user = userEvent.setup();
    renderPane({ workspaces: [{ number: 1, machine: ubuntuMachine, path: "/home/dev", label: "dev" }] });

    await user.click(within(await rowMenu("notes.txt")).getByRole("menuitem", { name: "删除…" }));

    expect(await screen.findByRole("alertdialog", { name: "确认删除" })).toHaveTextContent("永久删除“notes.txt”？");
  });

  it("makes a folder from the menu of the tree's empty space", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    fireEvent.contextMenu(container.querySelector(".files-pane__tree")!, { clientX: 40, clientY: 200 });
    const menu = await screen.findByRole("menu", { name: "文件操作" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "在文件管理器中打开",
      "在新页面打开",
      "新建目录",
      "复制绝对路径",
      "复制相对路径"
    ]);
    await user.click(within(menu).getByRole("menuitem", { name: "新建目录" }));
    await user.type(await screen.findByRole("textbox", { name: "新目录的名称" }), "notes{Enter}");

    await waitFor(() => expect(calls("browse_create_directory")).toEqual([{ machine: null, parent: ROOT, name: "notes" }]));
  });

  it("opens a page at any workspace from the tab strip's +", async () => {
    const user = userEvent.setup();
    renderPane({
      workspaces: [
        { number: 1, machine: null, path: ROOT, label: "mework" },
        { number: 2, machine: ubuntuMachine, path: "/srv", label: "srv" }
      ]
    });
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: "新建页面" }));
    const menu = await screen.findByRole("menu", { name: "新建页面" });
    expect(within(menu).getByText("本机")).toBeInTheDocument();
    expect(within(menu).getByText("ubuntu")).toBeInTheDocument();
    await user.click(within(menu).getByRole("menuitem", { name: /srv/ }));

    expect(await screen.findByRole("treeitem", { name: rowName("app") })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);
  });

  it("opens the only workspace straight from the + and moves the page from the footer's menu", async () => {
    const user = userEvent.setup();
    renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    await user.click(screen.getByRole("button", { name: "新建页面" }));
    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2));

    await user.dblClick(screen.getByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("button", { name: `位置：${ROOT}/src，点按编辑` });
    await user.click(screen.getByRole("button", { name: "工作区" }));
    await user.click(await screen.findByRole("menuitem", { name: /mework/ }));
    expect(await screen.findByRole("button", { name: `位置：${ROOT}，点按编辑` })).toBeInTheDocument();
  });

  async function tabMenu(name: RegExp) {
    fireEvent.contextMenu(screen.getByRole("tab", { name }), { clientX: 60, clientY: 10 });
    return screen.findByRole("menu", { name: "文件操作" });
  }

  it("opens a page's menu on a right click on its tab, with its file's own actions", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "在新页面打开" }));
    await screen.findByLabelText("readme.txt");
    // The tab carries no button for it.
    expect(screen.queryByRole("button", { name: /页面操作/ })).not.toBeInTheDocument();

    const menu = await tabMenu(/^readme\.txt/);
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "在文件树中显示",
      "在文件管理器中打开",
      "用…打开",
      "在新页面打开",
      "重命名…",
      "删除…",
      "复制绝对路径",
      "复制相对路径",
      "关闭页面",
      "关闭其他页面",
      "关闭所有页面"
    ]);
    await user.click(within(menu).getByRole("menuitem", { name: "复制绝对路径" }));
    expect(await navigator.clipboard.readText()).toBe(`${ROOT}/readme.txt`);

    const folder = await tabMenu(/^mework/);
    expect(within(folder).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "在文件管理器中打开",
      "用…打开",
      "在新页面打开",
      "新建目录",
      "重命名…",
      "删除…",
      "复制绝对路径",
      "复制相对路径",
      "关闭页面",
      "关闭其他页面",
      "关闭所有页面"
    ]);
  });

  it("renames a page's file in its tab and asks about a delete in its page", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "在新页面打开" }));
    await screen.findByLabelText("readme.txt");

    await user.click(within(await tabMenu(/^readme\.txt/)).getByRole("menuitem", { name: "重命名…" }));
    const field = await screen.findByRole("textbox", { name: "新名称" });
    expect(field.closest(".page-tab")).not.toBeNull();
    await user.clear(field);
    await user.type(field, "README.md{Enter}");
    await waitFor(() => expect(calls("browse_rename_path")).toEqual([{ machine: null, path: `${ROOT}/readme.txt`, name: "README.md" }]));
    expect(await screen.findByRole("tab", { name: /^README\.md/ })).toBeInTheDocument();

    await user.click(within(await tabMenu(/^README\.md/)).getByRole("menuitem", { name: "删除…" }));
    expect(await screen.findByRole("alertdialog", { name: "确认删除" })).toHaveTextContent("把“README.md”移到废纸篓？");
  });

  it("closes a page onto its neighbour and says so when none is left", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(within(await rowMenu("readme.txt")).getByRole("menuitem", { name: "在新页面打开" }));
    await screen.findByLabelText("readme.txt");

    await user.click(screen.getByRole("button", { name: "关闭 readme.txt" }));
    expect(await screen.findByRole("treeitem", { name: rowName("src") })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "关闭 mework" }));
    expect(await screen.findByText("没有打开的页面")).toBeInTheDocument();
    expect(screen.queryAllByRole("tab")).toHaveLength(0);
  });
});

/**
 * One file, one reading. What a file is decides what is made of it, and the
 * source toggle is the way back to the bytes for the two kinds that have another
 * form to show.
 */
describe("FilesPane viewer", () => {
  it("renders a Markdown file and gives it a way back to its source", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "# 标题\n\n正文一段。\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));

    expect(await screen.findByRole("heading", { name: "标题" })).toBeInTheDocument();
    expect(container.querySelector(".files-pane__code")).toBeNull();
    // Paths the document writes open on the document's machine.
    expect(container.querySelector(".files-pane__viewer")).toHaveAttribute("data-mework-path-machine", "local");

    await user.click(screen.getByRole("button", { name: "显示源码" }));

    const code = await screen.findByLabelText("Docs/guide.md");
    expect(code).toHaveClass("files-pane__code");
    expect(code).toHaveTextContent("# 标题");
    expect(screen.getByRole("button", { name: "显示渲染结果" })).toBeInTheDocument();
  });

  /** Plain text has only one reading, so it is never offered a switch between two. */
  it("offers no source toggle for a file that has only one form", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await screen.findByLabelText("readme.txt");

    expect(screen.queryByRole("button", { name: "显示源码" })).not.toBeInTheDocument();
  });

  it("colours a file it has a grammar for", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/src/App.tsx`] = textFile(`${ROOT}/src/App.tsx`, "const n = 1; // 注释\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");

    expect(container.querySelector(".code-token--keyword")).toHaveTextContent("const");
    expect(container.querySelector(".code-token--comment")).toHaveTextContent("// 注释");
    await user.click(screen.getByRole("treeitem", { name: rowName("readme.txt") }));
    await screen.findByLabelText("readme.txt");
    expect(container.querySelector(".code-token--keyword")).toBeNull();
  });

  it("shows a picture from its own bytes rather than calling it binary", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "logo.png"), entry(ROOT, "src", "directory")];
    files.local[`${ROOT}/logo.png`] = binaryFile(`${ROOT}/logo.png`);
    pictures.local[`${ROOT}/logo.png`] = pictureFile(`${ROOT}/logo.png`);
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("logo.png") }));

    const picture = await screen.findByRole("img", { name: "logo.png" });
    expect(picture).toHaveAttribute("src", `data:image/png;base64,${ONE_PIXEL_PNG}`);
    expect(screen.queryByText("二进制文件，无法显示")).not.toBeInTheDocument();
  });

  it("explains a picture the host refused to send whole", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "huge.png")];
    pictures.local[`${ROOT}/huge.png`] = { path: `${ROOT}/huge.png`, data: "", size: 90_000_000, tooLarge: true };
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("huge.png") }));

    expect(await screen.findByText("文件过大（超过 8 MB），无法在面板中预览。")).toBeInTheDocument();
    // What the pane cannot show it can still point at.
    expect(screen.getByRole("button", { name: "在文件管理器中显示" })).toBeInTheDocument();
  });

  /** A document's own pictures are read as bytes, because its `src` names a file next to it. */
  it("reads the pictures a rendered document points at", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "![图](./shots/a.png)\n");
    pictures.local[`${ROOT}/Docs/shots/a.png`] = pictureFile(`${ROOT}/Docs/shots/a.png`);
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));

    const picture = await screen.findByRole("img", { name: "图" });
    expect(picture).toHaveAttribute("src", `data:image/png;base64,${ONE_PIXEL_PNG}`);
  });

  /**
   * A relative `href` resolves against the app's own origin, so letting the
   * default through would navigate the whole window out of the app.
   */
  it("follows a relative link inside a document instead of navigating", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "见 [根说明](../readme.txt)。\n");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "根说明" }));

    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("# Mework");
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  /** Outside every workspace a file is still a file: the pane is not confined to one. */
  it("reads a file outside the workspace against the top of its filesystem", async () => {
    directories.local["/etc"] = [entry("/etc", "hosts")];
    files.local["/etc/hosts"] = textFile("/etc/hosts", "127.0.0.1 localhost\n");
    renderPane({ openRequest: { machine: null, path: "/etc/hosts", line: null, nonce: 1 } });

    expect(await screen.findByLabelText("etc/hosts")).toHaveTextContent("127.0.0.1 localhost");
    expect(await screen.findByRole("button", { name: "位置：/etc，点按编辑" })).toBeInTheDocument();
  });
});

/** What a click on a path elsewhere in the app turns into here. */
describe("FilesPane open requests", () => {
  it("opens the file a request names on a preview page in its workspace", async () => {
    const { rerender } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    rerender(<FilesPane {...fullProps({
      openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 1 }
    })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    const tab = screen.getByRole("tab", { name: /App\.tsx/ });
    expect(tab.closest(".page-tab")).toHaveClass("files-pane__tab--preview");
    // The pane's untouched first page became the file's page; the tree shows the file in place.
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toHaveAttribute("aria-current", "true");
  });

  it("reuses the preview page for the next request and keeps a kept one", async () => {
    const user = userEvent.setup();
    const { rerender } = renderPane({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 1 } });
    await screen.findByLabelText("src/App.tsx");

    rerender(<FilesPane {...fullProps({ openRequest: { machine: null, path: `${ROOT}/readme.txt`, line: null, nonce: 2 } })} />);
    await screen.findByLabelText("readme.txt");
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await user.dblClick(screen.getByRole("tab", { name: /readme\.txt/ }));
    expect(screen.getByRole("tab", { name: /readme\.txt/ }).closest(".page-tab")).not.toHaveClass("files-pane__tab--preview");

    rerender(<FilesPane {...fullProps({ openRequest: { machine: null, path: `${ROOT}/Docs/guide.md`, line: null, nonce: 3 } })} />);
    expect(await screen.findByRole("tab", { name: /guide\.md/ })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);
  });

  it("gives a file asked for on a page of its own a kept page beside the preview page", async () => {
    const { rerender } = renderPane({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 1 } });
    await screen.findByLabelText("src/App.tsx");

    rerender(<FilesPane {...fullProps({
      openRequest: { machine: null, path: `${ROOT}/readme.txt`, line: null, nonce: 2, newPage: true }
    })} />);
    await screen.findByLabelText("readme.txt");
    // The preview page keeps what it showed; the new page is not one to reuse.
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    expect(screen.getByRole("tab", { name: /App\.tsx/ }).closest(".page-tab")).toHaveClass("files-pane__tab--preview");
    expect(screen.getByRole("tab", { name: /readme\.txt/ }).closest(".page-tab")).not.toHaveClass("files-pane__tab--preview");

    // A page already showing the file is the one it opens on.
    rerender(<FilesPane {...fullProps({
      openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 3, newPage: true }
    })} />);
    await waitFor(() => expect(screen.getByRole("tab", { name: /App\.tsx/ })).toHaveAttribute("aria-selected", "true"));
    expect(screen.getAllByRole("tab")).toHaveLength(2);
  });

  it("opens a file on another machine", async () => {
    renderPane({ openRequest: { machine: ubuntuMachine, path: "/home/dev/notes.txt", line: null, nonce: 1 } });

    expect(await screen.findByLabelText("home/dev/notes.txt")).toHaveTextContent("远端笔记");
    expect(calls("browse_read_file")).toEqual([{ machine: ubuntuMachine, path: "/home/dev/notes.txt" }]);
  });

  /** A request that arrives while the pane is closed is honoured on the mount that follows. */
  it("honours a request that was already set at mount", async () => {
    renderPane({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 4 } });

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
  });

  it("lights the line a reference named", async () => {
    files.local[`${ROOT}/src/App.tsx`] = textFile(`${ROOT}/src/App.tsx`, "一\n二\n三\n");
    const { container } = renderPane({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: 2, nonce: 1 } });

    await screen.findByLabelText("src/App.tsx");
    await waitFor(() => expect(container.querySelector(".numbered-code__line--lit")).toHaveTextContent("二"));
  });

  /** A line is a place in the source, so a rendered document steps aside for it. */
  it("shows a document's source when the request names a line in it", async () => {
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "# 标题\n正文\n");
    renderPane({ openRequest: { machine: null, path: `${ROOT}/Docs/guide.md`, line: 1, nonce: 1 } });

    expect(await screen.findByLabelText("Docs/guide.md")).toHaveClass("files-pane__code");
  });

  it("asks again when the same file is requested twice", async () => {
    const user = userEvent.setup();
    const { rerender } = renderPane({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 1 } });
    await screen.findByLabelText("src/App.tsx");
    await user.click(screen.getByRole("button", { name: "关闭 App.tsx" }));
    await waitFor(() => expect(screen.queryAllByRole("tab")).toHaveLength(0));

    rerender(<FilesPane {...fullProps({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 2 } })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
  });
});

/** The handle beside the title that folds the tree away. */
describe("FilesPane tree drawer", () => {
  function treeColumn(container: HTMLElement): HTMLElement {
    return container.querySelector<HTMLElement>("[data-files-tree]")!;
  }

  it("sits right after the title and folds the tree away, with or without a file open", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    const toggle = screen.getByRole("button", { name: "收起文件目录" });
    expect(toggle.previousElementSibling).toHaveTextContent("文件");
    expect(toggle).toHaveAttribute("aria-expanded", "true");

    await user.click(toggle);
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(screen.getByText("打开文件树来浏览。")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "展开文件目录" }));
    expect(treeColumn(container)).not.toHaveAttribute("hidden");

    await user.click(screen.getByRole("treeitem", { name: rowName("readme.txt") }));
    await screen.findByLabelText("readme.txt");
    await user.click(screen.getByRole("button", { name: "收起文件目录" }));
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(screen.getByLabelText("readme.txt")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /readme\.txt/ })).toBeInTheDocument();
  });

  it("remembers the reader's choice for the next pane", async () => {
    const user = userEvent.setup();
    const first = renderPane();
    await user.click(await screen.findByRole("button", { name: "收起文件目录" }));
    first.unmount();

    const { container } = renderPane();
    await waitFor(() => expect(listingCalls().length).toBeGreaterThan(1));
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(screen.getByRole("button", { name: "展开文件目录" })).toBeInTheDocument();
  });

  /** A pane opened only to show a file from the timeline opens on the file alone. */
  it("folds the tree for a request that opened the pane, without changing the stored choice", async () => {
    const handled = vi.fn();
    const { container, unmount } = renderPane({
      openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 7, collapseTree: true },
      onOpenRequestHandled: handled
    });

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(handled).toHaveBeenCalledWith(7);
    unmount();

    const next = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });
    expect(treeColumn(next.container)).not.toHaveAttribute("hidden");
  });

  /** The mount that a request opens runs its effects twice under StrictMode; the read must still land. */
  it("finishes reading a requested file when the mount's effects run twice", async () => {
    render(
      <StrictMode>
        <FilesPane {...fullProps({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 3, collapseTree: true } })} />
      </StrictMode>
    );

    expect(await screen.findByLabelText("src/App.tsx")).toHaveTextContent("export {};");
  });

  it("keeps the tree as it was for a request made while the pane was open", async () => {
    const { container, rerender } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    rerender(<FilesPane {...fullProps({ openRequest: { machine: null, path: `${ROOT}/src/App.tsx`, line: null, nonce: 1, collapseTree: false } })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    expect(treeColumn(container)).not.toHaveAttribute("hidden");
  });
});

/** Links in a rendered document land where they point, not just on the file. */
describe("FilesPane document links", () => {
  it("opens a linked file at the line its anchor names", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "见 [第二行](../readme.txt#L2)。\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "第二行" }));

    await screen.findByLabelText("readme.txt");
    await waitFor(() => expect(container.querySelector(".numbered-code__line--lit")).toHaveTextContent("第二行"));
  });

  it("shows a linked directory in the tree instead of reading it as a file", async () => {
    const user = userEvent.setup();
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "看 [源码目录](../src/)。\n");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "源码目录" }));

    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: rowName("src") })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  it("renders the HTML a README carries, pictures included", async () => {
    files.local[`${ROOT}/Docs/guide.md`] = textFile(`${ROOT}/Docs/guide.md`, "<p align=\"center\"><img src=\"logo.png\" alt=\"标志\" width=\"80\"></p>\n\n<details><summary>更多</summary>\n\n隐藏内容\n\n</details>\n");
    pictures.local[`${ROOT}/Docs/logo.png`] = pictureFile(`${ROOT}/Docs/logo.png`);
    const user = userEvent.setup();
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));

    const logo = await screen.findByRole("img", { name: "标志" });
    expect(logo).toHaveAttribute("src", `data:image/png;base64,${ONE_PIXEL_PNG}`);
    expect(logo).toHaveAttribute("width", "80");
    expect(container.querySelector("details summary")).toHaveTextContent("更多");
  });
});

/** Every kind of file gets the viewer made for it. */
describe("FilesPane previewers", () => {
  async function open(user: ReturnType<typeof userEvent.setup>, name: string) {
    await user.click(await screen.findByRole("treeitem", { name: rowName(name) }));
  }

  it("draws an HTML page from its markup without running it", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "index.html"), entry(ROOT, "style.css")];
    files.local[`${ROOT}/index.html`] = textFile(
      `${ROOT}/index.html`,
      "<html><head><title>站点</title><link rel=\"stylesheet\" href=\"style.css\"><script>window.__ran = true</script></head>"
      + "<body class=\"home\"><h1 onclick=\"alert(1)\">你好</h1><iframe src=\"https://example.com\"></iframe><a href=\"other.html\">下一页</a></body></html>"
    );
    files.local[`${ROOT}/style.css`] = textFile(`${ROOT}/style.css`, "body { color: red; }");
    const { container } = renderPane();
    await open(user, "index.html");

    expect(await screen.findByText("静态预览：页面里的脚本不会运行。")).toBeInTheDocument();
    const host = container.querySelector<HTMLElement>(".file-preview__html-host")!;
    await waitFor(() => expect(host.shadowRoot?.querySelector("h1")).toHaveTextContent("你好"));
    const shadow = host.shadowRoot!;
    expect(shadow.querySelector("h1")).not.toHaveAttribute("onclick");
    expect(shadow.querySelector("script, iframe")).toBeNull();
    expect(shadow.querySelector(".mw-embed")).toHaveAttribute("data-kind", "iframe");
    expect(shadow.querySelector(".mw-html-body")).toHaveClass("home");
    expect((window as { __ran?: boolean }).__ran).toBeUndefined();
    // Its stylesheet is read from beside it on its machine, not fetched.
    await waitFor(() => expect(backend.invoke).toHaveBeenCalledWith("browse_read_file", { machine: null, path: `${ROOT}/style.css` }));
    expect(screen.getByRole("button", { name: "显示源码" })).toBeInTheDocument();
  });

  it("tabulates delimited text, numbers right-aligned", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "data.csv")];
    files.local[`${ROOT}/data.csv`] = textFile(`${ROOT}/data.csv`, "name,count\n\"Smith, J\",12\nLee,3\n");
    const { container } = renderPane();
    await open(user, "data.csv");

    const table = await screen.findByRole("table");
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["name", "count"]);
    expect(within(table).getByText("Smith, J")).toBeInTheDocument();
    expect(within(table).getByText("12")).toHaveClass("file-preview__csv-number");
    expect(container).toHaveTextContent("2 行 · 2 列");
  });

  it("reads a notebook the way Jupyter shows it", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "analysis.ipynb")];
    files.local[`${ROOT}/analysis.ipynb`] = textFile(`${ROOT}/analysis.ipynb`, JSON.stringify({
      nbformat: 4,
      metadata: { kernelspec: { language: "python" } },
      cells: [
        { cell_type: "markdown", source: ["# 分析\n", "说明"] },
        {
          cell_type: "code",
          execution_count: 3,
          source: "import math\nprint(math.pi)",
          outputs: [
            { output_type: "stream", name: "stdout", text: ["3.14159\n"] },
            { output_type: "execute_result", execution_count: 3, data: { "text/plain": "42", "image/png": ONE_PIXEL_PNG } },
            { output_type: "error", ename: "ValueError", evalue: "bad", traceback: ["\u001b[0;31mValueError\u001b[0m: bad"] }
          ]
        }
      ]
    }));
    const { container } = renderPane();
    await open(user, "analysis.ipynb");

    expect(await screen.findByRole("heading", { name: "分析" })).toBeInTheDocument();
    expect(container).toHaveTextContent("In [3]:");
    expect(container).toHaveTextContent("Out [3]:");
    expect(container.querySelector(".notebook__code .code-token--keyword")).toHaveTextContent("import");
    expect(screen.getByText("3.14159")).toBeInTheDocument();
    expect(container.querySelector(".notebook__image")).toHaveAttribute("src", `data:image/png;base64,${ONE_PIXEL_PNG}`);
    expect(container.querySelector(".notebook__text--error .ansi-fg-1")).toHaveTextContent("ValueError");
  });

  it("reads a PDF, a recording and a font as bytes, never as text", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "paper.pdf"), entry(ROOT, "voice.mp3"), entry(ROOT, "Inter.ttf")];
    for (const name of ["paper.pdf", "voice.mp3", "Inter.ttf"]) pictures.local[`${ROOT}/${name}`] = pictureFile(`${ROOT}/${name}`);
    renderPane();

    await open(user, "voice.mp3");
    // jsdom has no Web Audio; the player says so rather than failing silently.
    expect(await screen.findByText("当前界面引擎不支持音频解码。")).toBeInTheDocument();
    await open(user, "Inter.ttf");
    expect(await screen.findByText("当前界面引擎不支持字体预览。")).toBeInTheDocument();

    expect(calls("browse_read_file")).toEqual([]);
    expect(calls("browse_read_file_bytes").map((args) => args.path)).toEqual([`${ROOT}/voice.mp3`, `${ROOT}/Inter.ttf`]);
  });

  it("points at a video it cannot play instead of calling it binary", async () => {
    const user = userEvent.setup();
    directories.local[ROOT] = [entry(ROOT, "clip.mp4")];
    renderPane();
    await open(user, "clip.mp4");

    expect(await screen.findByText(/视频无法在应用内播放/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "在文件管理器中显示" }));
    expect(backend.invoke).toHaveBeenCalledWith("browse_open_in_file_manager", { path: `${ROOT}/clip.mp4` });
  });
});
