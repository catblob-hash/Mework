import { act, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GitTarget } from "../lib/git";
import type {
  WorkspaceEntry,
  WorkspaceEntryKind,
  WorkspaceFileBytes,
  WorkspaceFileContent,
  WorkspaceSearchResults
} from "../lib/workspaceFiles";
import { FilesPane } from "./FilesPane";
import type { FilesPaneProps } from "./FilesPane";

const backend = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => true),
  invoke: vi.fn()
}));

vi.mock("../lib/backend", () => backend);

const target: GitTarget = { kind: "conversation", conversationId: "conversation-1" };

let directories: Record<string, WorkspaceEntry[] | Error>;
let files: Record<string, WorkspaceFileContent | Error>;
let pictures: Record<string, WorkspaceFileBytes | Error>;
let searchResults: WorkspaceSearchResults | Error | null;

function entry(name: string, kind: WorkspaceEntryKind = "file"): WorkspaceEntry {
  return { name, kind, size: kind === "file" ? 12 : null };
}

function textFile(path: string, content: string): WorkspaceFileContent {
  return { path, content, truncated: false, size: content.length, binary: false };
}

function binaryFile(path: string): WorkspaceFileContent {
  return { path, content: "", truncated: false, size: 4096, binary: true };
}

/** A one-pixel PNG is enough: the viewer only ever hands the bytes to an `<img>`. */
const ONE_PIXEL_PNG = "iVBORw0KGgoAAAANSUhEUg==";

function pictureFile(path: string, data = ONE_PIXEL_PNG): WorkspaceFileBytes {
  return { path, data, size: 4096, tooLarge: false };
}

function listingCalls(): string[] {
  return backend.invoke.mock.calls
    .filter(([command]) => command === "list_workspace_directory")
    .map(([, args]) => (args as { relativePath: string }).relativePath);
}

function searchCalls(): string[] {
  return backend.invoke.mock.calls
    .filter(([command]) => command === "search_workspace_files")
    .map(([, args]) => (args as { query: string }).query);
}

/**
 * Every tree row carries a hidden `⋮` menu, so a row's accessible name is its
 * label plus the menu's own label; these anchors match the name's head.
 */
function rowName(name: string): RegExp {
  return new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
}

/**
 * Directory and file names are user data, so the fixtures are looked up by own
 * key: a workspace holding `constructor` must not answer with something the
 * prototype chain happens to carry.
 */
function fixture<T>(table: Record<string, T>, key: string): T | undefined {
  return Object.prototype.hasOwnProperty.call(table, key) ? table[key] : undefined;
}

interface DeferredRead {
  resolve: (file: WorkspaceFileContent) => void;
  reject: (error: Error) => void;
}

/** The root answers at once; every file read is held so the test settles them in its own order. */
function holdFileReads(rootEntries: WorkspaceEntry[]): DeferredRead[] {
  const reads: DeferredRead[] = [];
  backend.invoke.mockImplementation((command: string, args: Record<string, unknown>) => {
    if (command === "list_workspace_directory") {
      const relativePath = args.relativePath as string;
      return relativePath === ""
        ? Promise.resolve({ path: "", entries: rootEntries })
        : Promise.reject(new Error(`未知目录：${relativePath}`));
    }
    if (command !== "read_workspace_file") return Promise.reject(new Error(`未预期的命令：${command}`));
    return new Promise<WorkspaceFileContent>((resolve, reject) => { reads.push({ resolve, reject }); });
  });
  return reads;
}

const workspacePath = "C:\\Projects\\Mework";

function fullProps(overrides: Partial<FilesPaneProps> = {}): FilesPaneProps {
  return {
    paneId: "files",
    target,
    rootLabel: "Mework",
    workspacePath,
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
  backend.hasBackendRuntime.mockReturnValue(true);
  backend.invoke.mockReset();
  directories = {
    "": [entry("readme.txt"), entry("src", "directory"), entry("Docs", "directory")],
    src: [entry("lib", "directory"), entry("App.tsx")],
    Docs: [entry("guide.md")]
  };
  files = {
    "readme.txt": textFile("readme.txt", "# Mework\n第二行\n"),
    "src/App.tsx": textFile("src/App.tsx", "export {};\n"),
    "Docs/guide.md": textFile("Docs/guide.md", "指南\n")
  };
  pictures = {};
  searchResults = null;
  backend.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
    const relativePath = args.relativePath as string;
    if (command === "list_workspace_directory") {
      const entries = fixture(directories, relativePath);
      if (entries === undefined) throw new Error(`未知目录：${relativePath}`);
      if (entries instanceof Error) throw entries;
      return { path: relativePath, entries };
    }
    if (command === "read_workspace_file") {
      const file = fixture(files, relativePath);
      if (file === undefined) throw new Error(`未知文件：${relativePath}`);
      if (file instanceof Error) throw file;
      return file;
    }
    if (command === "read_workspace_file_bytes") {
      const picture = fixture(pictures, relativePath);
      if (picture === undefined) throw new Error(`未知文件：${relativePath}`);
      if (picture instanceof Error) throw picture;
      return picture;
    }
    if (command === "search_workspace_files") {
      if (searchResults === null) throw new Error("未配置的搜索结果");
      if (searchResults instanceof Error) throw searchResults;
      return searchResults;
    }
    throw new Error(`未预期的命令：${command}`);
  });
});

describe("FilesPane", () => {
  /** The host sorts; the pane must not undo that order on its way to the screen. */
  it("lists the workspace root with directories first", async () => {
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    const rows = screen.getAllByRole("treeitem");
    expect(rows.map((row) => row.textContent)).toEqual(["Docs", "src", "readme.txt"]);
    expect(listingCalls()).toEqual([""]);
  });

  it("shows the workspace name in the title bar until a file is open", async () => {
    const { container } = renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    const paneTitle = container.querySelector(".files-pane__pane-title");
    expect(paneTitle).toHaveTextContent("文件");
    expect(paneTitle).toHaveAttribute("title", workspacePath);
  });

  it("lazy-loads a directory on expansion and keeps the listing", async () => {
    const user = userEvent.setup();
    renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    expect(listingCalls()).toEqual([""]);

    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("App.tsx") });
    expect(listingCalls()).toEqual(["", "src"]);
    expect(screen.getByRole("treeitem", { name: rowName("src") })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("treeitem", { name: rowName("App.tsx") })).toHaveAttribute("aria-level", "2");

    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    expect(screen.queryByRole("treeitem", { name: rowName("App.tsx") })).not.toBeInTheDocument();
    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("App.tsx") });
    expect(listingCalls()).toEqual(["", "src"]);
  });

  /** Directory names are user data: a folder called `constructor` is a folder, not a prototype member. */
  it("expands a directory whose name is also an Object.prototype member", async () => {
    const user = userEvent.setup();
    directories = {
      "": [entry("constructor", "directory"), entry("readme.txt")],
      constructor: [entry("valueOf.ts")]
    };
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("constructor") }));

    expect(await screen.findByRole("treeitem", { name: rowName("valueOf.ts") })).toBeInTheDocument();
    expect(listingCalls()).toEqual(["", "constructor"]);
    expect(screen.getByRole("treeitem", { name: rowName("constructor") })).toHaveAttribute("aria-expanded", "true");
  });

  it("opens a file in a line-numbered viewer as a preview tab", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    const code = await screen.findByLabelText("readme.txt");
    expect(code).toHaveClass("files-pane__code");
    const numbers = Array.from(
      container.querySelectorAll(".numbered-code__number"),
      (node) => node.textContent
    );
    expect(numbers).toEqual(["1", "2"]);
    const lines = Array.from(
      container.querySelectorAll(".numbered-code__text"),
      (node) => node.textContent
    );
    expect(lines).toEqual(["# Mework", "第二行"]);
    const tab = screen.getByRole("tab", { name: /readme\.txt/ });
    expect(tab.closest(".page-tab")).toHaveClass("files-pane__tab--preview");
  });

  it("keeps the tree beside the viewer once a file is open", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");

    expect(screen.getByRole("tree")).toBeInTheDocument();
    const treeColumn = container.querySelector("[data-files-tree]");
    expect(treeColumn).not.toHaveAttribute("hidden");
    expect(screen.getByRole("tab", { name: /App\.tsx/ })).toBeInTheDocument();
    // Opening a file no longer re-reads the tree; the listing the user built stays.
    expect(listingCalls()).toEqual(["", "src"]);
  });

  it("says a binary file cannot be shown instead of rendering it", async () => {
    const user = userEvent.setup();
    files["readme.txt"] = { path: "readme.txt", content: "", truncated: false, size: 4096, binary: true };
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByText("二进制文件，无法显示")).toBeInTheDocument();
    expect(container.querySelector(".files-pane__code")).toBeNull();
  });

  it("warns that a truncated file is missing its tail", async () => {
    const user = userEvent.setup();
    files["readme.txt"] = { path: "readme.txt", content: "开头", truncated: true, size: 2_000_000, binary: false };
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByText("文件过大，只显示了开头部分")).toBeInTheDocument();
    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("开头");
  });

  /** A failed child keeps the rest of the tree: only that directory reports the failure. */
  it("puts a rejected child listing in its own row", async () => {
    const user = userEvent.setup();
    directories.src = new Error("文件浏览的路径不在工作区内");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));

    expect(await screen.findByText("文件浏览的路径不在工作区内")).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: rowName("readme.txt") })).toBeInTheDocument();
  });

  it("replaces the tree when the root itself cannot be read", async () => {
    directories[""] = new Error("无法读取这个文件夹");
    renderPane();

    expect(await screen.findByRole("alert")).toHaveTextContent("无法读取这个文件夹");
    expect(screen.queryByRole("tree")).not.toBeInTheDocument();
  });

  it("shows the host's message when a file cannot be read", async () => {
    const user = userEvent.setup();
    files["readme.txt"] = new Error("这个文件已经不在了");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));

    expect(await screen.findByRole("alert")).toHaveTextContent("这个文件已经不在了");
  });

  /** Closing and reopening leaves two reads in flight; the older one is no longer the answer. */
  it("keeps the newer read when an earlier read of the same file lands late", async () => {
    const user = userEvent.setup();
    const reads = holdFileReads([entry("readme.txt")]);
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await user.click(await screen.findByRole("button", { name: "关闭 readme.txt" }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await waitFor(() => expect(reads).toHaveLength(2));

    await act(async () => { reads[1]!.resolve(textFile("readme.txt", "现在的内容\n")); });
    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("现在的内容");

    await act(async () => { reads[0]!.resolve(textFile("readme.txt", "过时的内容\n")); });

    expect(screen.getByLabelText("readme.txt")).toHaveTextContent("现在的内容");
    expect(screen.getByLabelText("readme.txt")).not.toHaveTextContent("过时的内容");
  });

  it("keeps the newer read when an earlier read of the same file fails late", async () => {
    const user = userEvent.setup();
    const reads = holdFileReads([entry("readme.txt")]);
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await user.click(await screen.findByRole("button", { name: "关闭 readme.txt" }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await waitFor(() => expect(reads).toHaveLength(2));

    await act(async () => { reads[1]!.resolve(textFile("readme.txt", "现在的内容\n")); });
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
    expect(listingCalls()).toEqual(["", "src"]);

    files["src/App.tsx"] = textFile("src/App.tsx", "export const refreshed = true;\n");
    await user.click(screen.getByRole("button", { name: "文件 设置" }));
    await user.click(await screen.findByRole("menuitem", { name: "刷新" }));

    await waitFor(() => expect(listingCalls()).toEqual(["", "src", "", "src"]));
    expect(await screen.findByLabelText("src/App.tsx")).toHaveTextContent("export const refreshed = true;");
  });

  /** A refresh abandons the answers in flight; the directories waiting for them must not stay stuck. */
  it("re-requests a directory whose pending listing the refresh threw away", async () => {
    const user = userEvent.setup();
    const rootEntries = [entry("src", "directory")];
    const sourceEntries = [entry("App.tsx")];
    let releaseFirst: (listing: { path: string; entries: WorkspaceEntry[] }) => void = () => {};
    let sourceRequests = 0;
    backend.invoke.mockImplementation((command: string, args: Record<string, unknown>) => {
      if (command !== "list_workspace_directory") return Promise.reject(new Error(`未预期的命令：${command}`));
      const relativePath = args.relativePath as string;
      if (relativePath === "") return Promise.resolve({ path: "", entries: rootEntries });
      if (relativePath !== "src") return Promise.reject(new Error(`未知目录：${relativePath}`));
      sourceRequests += 1;
      if (sourceRequests > 1) return Promise.resolve({ path: "src", entries: sourceEntries });
      return new Promise((resolve) => { releaseFirst = resolve; });
    });
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    expect(sourceRequests).toBe(1);
    expect(screen.getByText("正在读取…")).toBeInTheDocument();

    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await user.click(screen.getByRole("button", { name: "文件 设置" }));
    await user.click(await screen.findByRole("menuitem", { name: "刷新" }));
    await act(async () => { releaseFirst({ path: "src", entries: sourceEntries }); });

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));

    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(sourceRequests).toBe(2);
  });

  it("re-reads the root when the target moves to another workspace", async () => {
    const { rerender } = renderPane();

    await screen.findByRole("treeitem", { name: rowName("src") });
    expect(listingCalls()).toEqual([""]);

    rerender(<FilesPane {...fullProps({ target: { kind: "workspace", workspaceId: "workspace-2" } })} />);

    await waitFor(() => expect(listingCalls()).toEqual(["", ""]));
  });

  /** The same relative path names another file in another workspace; nothing cached may survive. */
  it("forgets a collapsed directory's listing when the target moves to another workspace", async () => {
    const user = userEvent.setup();
    const trees: Record<string, Record<string, WorkspaceEntry[]>> = {
      "conversation:conversation-1": { "": [entry("src", "directory")], src: [entry("old.ts")] },
      "workspace:workspace-2": { "": [entry("src", "directory")], src: [entry("new.ts")] }
    };
    backend.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
      if (command !== "list_workspace_directory") throw new Error(`未预期的命令：${command}`);
      const requested = args.target as GitTarget;
      const key = requested.kind === "conversation"
        ? `conversation:${requested.conversationId}`
        : `workspace:${requested.workspaceId}`;
      const relativePath = args.relativePath as string;
      const entries = fixture(trees[key] ?? {}, relativePath);
      if (entries === undefined) throw new Error(`未知目录：${relativePath}`);
      return { path: relativePath, entries };
    });
    const { rerender } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await screen.findByRole("treeitem", { name: rowName("old.ts") });
    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));

    rerender(<FilesPane {...fullProps({ target: { kind: "workspace", workspaceId: "workspace-2" } })} />);

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));

    expect(await screen.findByRole("treeitem", { name: rowName("new.ts") })).toBeInTheDocument();
    expect(screen.queryByRole("treeitem", { name: rowName("old.ts") })).not.toBeInTheDocument();
    expect(listingCalls()).toEqual(["", "src", "", "src"]);
  });

  /**
   * A single click opens the one reusable preview slot; a double click promotes
   * the tab, and the next single click then opens beside it.
   */
  it("reuses the preview tab for single clicks and promotes it on double click", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    const preview = await screen.findByRole("tab", { name: /readme\.txt/ });
    expect(preview.closest(".page-tab")).toHaveClass("files-pane__tab--preview");
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    expect(screen.queryByRole("tab", { name: /readme\.txt/ })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /App\.tsx/ }).closest(".page-tab"))
      .toHaveClass("files-pane__tab--preview");

    await user.dblClick(screen.getByRole("treeitem", { name: rowName("readme.txt") }));
    const promoted = screen.getByRole("tab", { name: /readme\.txt/ });
    expect(promoted.closest(".page-tab")).not.toHaveClass("files-pane__tab--preview");

    await user.click(screen.getByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    expect(await screen.findByRole("tab", { name: /guide\.md/ })).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);
  });

  it("closes a tab onto its neighbour and drops the strip with the last tab", async () => {
    const user = userEvent.setup();
    renderPane();

    await user.dblClick(await screen.findByRole("treeitem", { name: rowName("readme.txt") }));
    await user.click(screen.getByRole("treeitem", { name: rowName("src") }));
    await user.dblClick(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");
    expect(screen.getByRole("tablist", { name: "打开的文件" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "关闭 App.tsx" }));
    expect(await screen.findByLabelText("readme.txt")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "关闭 readme.txt" }));
    await waitFor(() => expect(screen.queryByRole("tablist")).not.toBeInTheDocument());
    expect(screen.getByText("打开的文件会出现在这里")).toBeInTheDocument();
  });

  it("searches the workspace from the filter and opens a match", async () => {
    const user = userEvent.setup();
    searchResults = {
      query: "app",
      matches: [
        { name: "App.tsx", path: "src/App.tsx", kind: "file", positions: [4, 5, 6], score: 1 }
      ],
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
      "search_workspace_files",
      expect.objectContaining({ query: "app", limit: 200 })
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
      expect(screen.getAllByRole("treeitem").map((row) => row.getAttribute("tabindex")))
        .toEqual(["-1", "-1", "0"]);
    });
  });

  it("waits for the pane to be shown before touching the host", async () => {
    const { rerender } = renderPane({ active: false });

    expect(backend.invoke).not.toHaveBeenCalled();

    rerender(<FilesPane {...fullProps({ workspacePath: null })} />);

    expect(await screen.findByRole("treeitem", { name: rowName("src") })).toBeInTheDocument();
  });

  it("shows a loading row until the root answers", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const rootEntries = directories[""];
    backend.invoke.mockImplementation(async (command: string) => {
      if (command !== "list_workspace_directory") throw new Error("未预期的命令");
      await gate;
      return { path: "", entries: rootEntries };
    });
    renderPane();

    expect(screen.getByText("正在读取…")).toBeInTheDocument();
    release();

    expect(await screen.findByRole("treeitem", { name: rowName("src") })).toBeInTheDocument();
  });

  it("says so when a folder holds nothing", async () => {
    directories.Docs = [];
    const user = userEvent.setup();
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));

    const tree = screen.getByRole("tree");
    expect(await within(tree).findByText("文件夹为空")).toBeInTheDocument();
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
    files["Docs/guide.md"] = textFile("Docs/guide.md", "# 标题\n\n正文一段。\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));

    expect(await screen.findByRole("heading", { name: "标题" })).toBeInTheDocument();
    expect(container.querySelector(".files-pane__code")).toBeNull();

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
    files["src/App.tsx"] = textFile("src/App.tsx", "const n = 1; // 注释\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("src") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("App.tsx") }));
    await screen.findByLabelText("src/App.tsx");

    expect(container.querySelector(".code-token--keyword")).toHaveTextContent("const");
    expect(container.querySelector(".code-token--comment")).toHaveTextContent("// 注释");
    // A file with no grammar keeps the same line rows, uncoloured.
    await user.click(screen.getByRole("treeitem", { name: rowName("readme.txt") }));
    await screen.findByLabelText("readme.txt");
    expect(container.querySelector(".code-token--keyword")).toBeNull();
  });

  it("shows a picture from its own bytes rather than calling it binary", async () => {
    const user = userEvent.setup();
    directories[""] = [entry("logo.png"), entry("src", "directory")];
    files["logo.png"] = binaryFile("logo.png");
    pictures["logo.png"] = pictureFile("logo.png");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("logo.png") }));

    const picture = await screen.findByRole("img", { name: "logo.png" });
    expect(picture).toHaveAttribute("src", `data:image/png;base64,${ONE_PIXEL_PNG}`);
    expect(screen.queryByText("二进制文件，无法显示")).not.toBeInTheDocument();
  });

  it("explains a picture the host refused to send whole", async () => {
    const user = userEvent.setup();
    directories[""] = [entry("huge.png")];
    files["huge.png"] = binaryFile("huge.png");
    pictures["huge.png"] = { path: "huge.png", data: "", size: 90_000_000, tooLarge: true };
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("huge.png") }));

    expect(await screen.findByText("文件过大（超过 8 MB），无法在面板中预览。")).toBeInTheDocument();
    // What the pane cannot show it can still point at.
    expect(screen.getByRole("button", { name: "在文件管理器中显示" })).toBeInTheDocument();
  });

  /** A document's own pictures are read as bytes, because its `src` is a workspace path. */
  it("reads the pictures a rendered document points at", async () => {
    const user = userEvent.setup();
    files["Docs/guide.md"] = textFile("Docs/guide.md", "![图](./shots/a.png)\n");
    pictures["Docs/shots/a.png"] = pictureFile("Docs/shots/a.png");
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
    files["Docs/guide.md"] = textFile("Docs/guide.md", "见 [根说明](../readme.txt)。\n");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "根说明" }));

    expect(await screen.findByLabelText("readme.txt")).toHaveTextContent("# Mework");
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });
});

/** What a click on a path elsewhere in the app turns into here. */
describe("FilesPane open requests", () => {
  it("opens the file a request names as a preview tab", async () => {
    const { rerender } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    rerender(<FilesPane {...fullProps({
      openRequest: { path: "src/App.tsx", line: null, nonce: 1 }
    })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    const tab = screen.getByRole("tab", { name: /App\.tsx/ });
    expect(tab.closest(".page-tab")).toHaveClass("files-pane__tab--preview");
  });

  /** A request that arrives while the pane is closed is honoured on the mount that follows. */
  it("honours a request that was already set at mount", async () => {
    renderPane({ openRequest: { path: "src/App.tsx", line: null, nonce: 4 } });

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
  });

  it("lights the line a reference named", async () => {
    files["src/App.tsx"] = textFile("src/App.tsx", "一\n二\n三\n");
    const { container } = renderPane({
      openRequest: { path: "src/App.tsx", line: 2, nonce: 1 }
    });

    await screen.findByLabelText("src/App.tsx");
    await waitFor(() => {
      expect(container.querySelector(".numbered-code__line--lit")).toHaveTextContent("二");
    });
  });

  /** A line is a place in the source, so a rendered document steps aside for it. */
  it("shows a document's source when the request names a line in it", async () => {
    files["Docs/guide.md"] = textFile("Docs/guide.md", "# 标题\n正文\n");
    renderPane({ openRequest: { path: "Docs/guide.md", line: 1, nonce: 1 } });

    expect(await screen.findByLabelText("Docs/guide.md")).toHaveClass("files-pane__code");
  });

  it("asks again when the same file is requested twice", async () => {
    const user = userEvent.setup();
    const { rerender } = renderPane({
      openRequest: { path: "src/App.tsx", line: null, nonce: 1 }
    });
    await screen.findByLabelText("src/App.tsx");
    await user.click(screen.getByRole("button", { name: "关闭 App.tsx" }));
    await waitFor(() => expect(screen.queryByRole("tablist")).not.toBeInTheDocument());

    rerender(<FilesPane {...fullProps({
      openRequest: { path: "src/App.tsx", line: null, nonce: 2 }
    })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
  });
});

/** The handle beside the title that folds the tree away. */
describe("FilesPane tree drawer", () => {
  beforeEach(() => window.localStorage.clear());

  function treeColumn(container: HTMLElement): HTMLElement {
    return container.querySelector<HTMLElement>("[data-files-tree]")!;
  }

  it("sits right after the title and folds the tree away, with or without a file open", async () => {
    const user = userEvent.setup();
    const { container } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    const toggle = screen.getByRole("button", { name: "收起文件目录" });
    // The title and the handle are neighbours in the title bar.
    expect(toggle.previousElementSibling).toHaveTextContent("文件");
    expect(toggle).toHaveAttribute("aria-expanded", "true");

    await user.click(toggle);
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(screen.getByText("打开文件树来浏览这个工作区。")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "展开文件目录" }));
    expect(treeColumn(container)).not.toHaveAttribute("hidden");

    await user.click(screen.getByRole("treeitem", { name: rowName("readme.txt") }));
    await screen.findByLabelText("readme.txt");
    await user.click(screen.getByRole("button", { name: "收起文件目录" }));
    expect(treeColumn(container)).toHaveAttribute("hidden");
    // The file keeps the pane; its tab strip follows the title and the handle.
    expect(screen.getByLabelText("readme.txt")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /readme\.txt/ })).toBeInTheDocument();
  });

  it("remembers the reader's choice for the next pane", async () => {
    const user = userEvent.setup();
    const first = renderPane();
    await user.click(await screen.findByRole("button", { name: "收起文件目录" }));
    first.unmount();

    const { container } = renderPane();
    await waitFor(() => expect(listingCalls().length).toBeGreaterThan(0));
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(screen.getByRole("button", { name: "展开文件目录" })).toBeInTheDocument();
  });

  /** A pane opened only to show a file from the timeline opens on the file alone. */
  it("folds the tree for a request that opened the pane, without changing the stored choice", async () => {
    const handled = vi.fn();
    const { container, unmount } = renderPane({
      openRequest: { path: "src/App.tsx", line: null, nonce: 7, collapseTree: true },
      onOpenRequestHandled: handled
    });

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    expect(treeColumn(container)).toHaveAttribute("hidden");
    expect(handled).toHaveBeenCalledWith(7);
    unmount();

    // The next pane opened by hand shows the tree the reader asked for.
    const next = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });
    expect(treeColumn(next.container)).not.toHaveAttribute("hidden");
  });

  /**
   * The mount that a request opens runs its effects twice under StrictMode; the
   * second reload must not fence off the read the request started.
   */
  it("finishes reading a requested file when the mount's effects run twice", async () => {
    render(
      <StrictMode>
        <FilesPane {...fullProps({ openRequest: { path: "src/App.tsx", line: null, nonce: 3, collapseTree: true } })} />
      </StrictMode>
    );

    expect(await screen.findByLabelText("src/App.tsx")).toHaveTextContent("export {};");
  });

  it("keeps the tree as it was for a request made while the pane was open", async () => {
    const { container, rerender } = renderPane();
    await screen.findByRole("treeitem", { name: rowName("src") });

    rerender(<FilesPane {...fullProps({ openRequest: { path: "src/App.tsx", line: null, nonce: 1, collapseTree: false } })} />);

    expect(await screen.findByLabelText("src/App.tsx")).toBeInTheDocument();
    expect(treeColumn(container)).not.toHaveAttribute("hidden");
  });
});

/** Links in a rendered document land where they point, not just on the file. */
describe("FilesPane document links", () => {
  it("opens a linked file at the line its anchor names", async () => {
    const user = userEvent.setup();
    files["Docs/guide.md"] = textFile("Docs/guide.md", "见 [第二行](../readme.txt#L2)。\n");
    const { container } = renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "第二行" }));

    await screen.findByLabelText("readme.txt");
    await waitFor(() => expect(container.querySelector(".numbered-code__line--lit")).toHaveTextContent("第二行"));
  });

  it("shows a linked directory in the tree instead of reading it as a file", async () => {
    const user = userEvent.setup();
    files["Docs/guide.md"] = textFile("Docs/guide.md", "看 [源码目录](../src/)。\n");
    renderPane();

    await user.click(await screen.findByRole("treeitem", { name: rowName("Docs") }));
    await user.click(await screen.findByRole("treeitem", { name: rowName("guide.md") }));
    await user.click(await screen.findByRole("link", { name: "源码目录" }));

    expect(await screen.findByRole("treeitem", { name: rowName("App.tsx") })).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: rowName("src") })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  it("renders the HTML a README carries, pictures included", async () => {
    files["Docs/guide.md"] = textFile("Docs/guide.md", "<p align=\"center\"><img src=\"logo.png\" alt=\"标志\" width=\"80\"></p>\n\n<details><summary>更多</summary>\n\n隐藏内容\n\n</details>\n");
    pictures["Docs/logo.png"] = pictureFile("Docs/logo.png");
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
    directories[""] = [entry("index.html"), entry("style.css")];
    files["index.html"] = textFile(
      "index.html",
      "<html><head><title>站点</title><link rel=\"stylesheet\" href=\"style.css\"><script>window.__ran = true</script></head>"
      + "<body class=\"home\"><h1 onclick=\"alert(1)\">你好</h1><iframe src=\"https://example.com\"></iframe><a href=\"other.html\">下一页</a></body></html>"
    );
    files["style.css"] = textFile("style.css", "body { color: red; }");
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
    // Its stylesheet is read from the workspace, not fetched.
    await waitFor(() => expect(backend.invoke).toHaveBeenCalledWith("read_workspace_file", { target, relativePath: "style.css" }));
    expect(screen.getByRole("button", { name: "显示源码" })).toBeInTheDocument();
  });

  it("tabulates delimited text, numbers right-aligned", async () => {
    const user = userEvent.setup();
    directories[""] = [entry("data.csv")];
    files["data.csv"] = textFile("data.csv", "name,count\n\"Smith, J\",12\nLee,3\n");
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
    directories[""] = [entry("analysis.ipynb")];
    files["analysis.ipynb"] = textFile("analysis.ipynb", JSON.stringify({
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
    directories[""] = [entry("paper.pdf"), entry("voice.mp3"), entry("Inter.ttf")];
    for (const name of ["paper.pdf", "voice.mp3", "Inter.ttf"]) pictures[name] = pictureFile(name);
    renderPane();

    await open(user, "voice.mp3");
    // jsdom has no Web Audio; the player says so rather than failing silently.
    expect(await screen.findByText("当前界面引擎不支持音频解码。")).toBeInTheDocument();
    await open(user, "Inter.ttf");
    expect(await screen.findByText("当前界面引擎不支持字体预览。")).toBeInTheDocument();

    const textReads = backend.invoke.mock.calls.filter(([command]) => command === "read_workspace_file");
    expect(textReads).toEqual([]);
    const byteReads = backend.invoke.mock.calls
      .filter(([command]) => command === "read_workspace_file_bytes")
      .map(([, args]) => (args as { relativePath: string }).relativePath);
    expect(byteReads).toEqual(["voice.mp3", "Inter.ttf"]);
  });

  it("points at a video it cannot play instead of calling it binary", async () => {
    const user = userEvent.setup();
    directories[""] = [entry("clip.mp4")];
    backend.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
      if (command === "list_workspace_directory") return { path: "", entries: directories[""] };
      if (command === "reveal_path_in_file_manager") return undefined;
      throw new Error(`未预期的命令：${command} ${JSON.stringify(args)}`);
    });
    renderPane();
    await open(user, "clip.mp4");

    expect(await screen.findByText(/视频无法在应用内播放/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "在文件管理器中显示" }));
    expect(backend.invoke).toHaveBeenCalledWith("reveal_path_in_file_manager", { path: "clip.mp4", baseDir: workspacePath });
  });
});
