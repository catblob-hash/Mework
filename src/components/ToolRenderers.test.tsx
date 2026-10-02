import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Plug, Wrench } from "lucide-react";
import { describe, expect, it } from "vitest";
import { translate } from "../i18n";
import { prepareImageAttachment } from "../lib/runtime";
import { toolCatalog } from "../seed";
import type { ToolContext, ToolDescriptor } from "../types";
import {
  errorExcerpt,
  getToolPresentation,
  isMcpToolName,
  mcpToolNaming,
  summarizeBlockKinds,
  TOOL_VIEW_REGISTRY,
  ToolDetailRenderer,
  toolRowName,
  toolSummaryKind,
  toolSurfaceForName
} from "./ToolRenderers";

const PREVIEW_PIXEL_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
const PREVIEW_PIXEL_DATA_URL = `data:image/png;base64,${PREVIEW_PIXEL_BASE64}`;

async function previewPixel(name: string) {
  const bytes = Uint8Array.from(
    window.atob(PREVIEW_PIXEL_BASE64),
    (character) => character.charCodeAt(0)
  );
  return prepareImageAttachment(name, bytes);
}

function tool(
  toolName: string,
  input: ToolContext["input"],
  output: string,
  options: { success?: boolean; diff?: string } = {}
): ToolContext {
  return {
    id: `tool-${toolName}`,
    kind: "tool",
    toolName,
    round: 1,
    input,
    result: {
      success: options.success ?? true,
      output,
      ...(options.diff ? { diff: options.diff } : {}),
      executedAt: "2026-07-14T00:00:00Z",
      durationMs: 12
    },
    createdAt: "2026-07-14T00:00:00Z"
  };
}

/**
 * A key/value card read back as label → text, in the order it drew them. The
 * result row carries no label, so it keys on the empty string.
 */
function kvRows(container: HTMLElement): Map<string, string> {
  return new Map([...container.querySelectorAll(".tool-kv__row")].map((row) => [
    row.querySelector(".tool-kv__key")?.textContent ?? "",
    row.querySelector(".tool-kv__value")?.textContent ?? ""
  ]));
}

/**
 * The host names an MCP tool `mcp__<serverSlug>__<toolSlug>__<digest>`, and the
 * server slug carries a digest of its own. Both are written out here so the
 * fallback parser is tested against the shape the backend actually emits.
 */
const MCP_WIRE_NAME = "mcp__playwright_1a2b3c4d5e__browser_click__0f9e8d7c6b";

function mcpDescriptor(label: string, name = MCP_WIRE_NAME): ToolDescriptor {
  return { name, label, description: "", category: "mcp", dangerous: false, parameters: [] };
}

describe("handoff rows", () => {
  const en = (zh: string, english: string, values?: Record<string, string | number>) =>
    translate("en-US", zh, english, values);

  it("names the host's handoff notice apart from a background result", () => {
    const notice = {
      ...tool(
        "box",
        { none: [] },
        "<task-notification>\n<summary>past the threshold</summary>\n<result>\nHand this conversation off.\n</result>\n</task-notification>"
      ),
      notice: "handoff"
    };
    expect(getToolPresentation(notice, undefined, en).title)
      .toBe("Context reached the auto-compact threshold; handoff requested");
    const result = tool("box", { none: [] }, "<task-notification>\n<task-id>reviewer</task-id>\n</task-notification>");
    expect(getToolPresentation(result, undefined, en).title).toBe("Delivered a background result");
    // A card written before the card held the whole message named its kind in its input.
    const legacy = tool(
      "box",
      { tasks: [], notification: { kind: "handoff", summary: "past the threshold" } },
      "Hand this conversation off before its context runs out."
    );
    expect(getToolPresentation(legacy, undefined, en).title)
      .toBe("Context reached the auto-compact threshold; handoff requested");
  });

  it("opens a written note onto its name, index line and body", () => {
    const note = tool(
      "create_handoff_note",
      { name: "state", description: "where the work stands", content: "Step 2 of 3." },
      "Created handoff note state.md and recorded it in the handoff index."
    );
    const presentation = getToolPresentation(note, undefined, en);
    expect(presentation.title).toBe("Wrote handoff note");
    expect(presentation.target).toBe("state");
    expect(presentation.keys).toEqual(["name", "description", "content"]);
    expect(toolSummaryKind(note)).toBe("handoff");
  });
});

describe("shell command explanations", () => {
  const en = (zh: string, en: string, params?: Record<string, string | number>) => translate("en-US", zh, en, params);
  const zh = (zh: string, en: string, params?: Record<string, string | number>) => translate("zh-CN", zh, en, params);

  it("replaces a shell card's title with the shell and the explanation", () => {
    const item = tool("bash", { command: "ls -la" }, "total 0");
    expect(getToolPresentation(item, undefined, en, "List directory contents").title).toBe("Bash: List directory contents");
    expect(getToolPresentation(item, undefined, zh, "列出文件详细信息").title).toBe("Bash：列出文件详细信息");
    expect(getToolPresentation(tool("powershell", { command: "Get-Process" }, ""), undefined, en, "List processes").title)
      .toBe("PowerShell: List processes");
  });

  it("keeps the original title without an explanation, and marks failures", () => {
    const item = tool("zsh", { command: "false" }, "", { success: false });
    expect(getToolPresentation(item, undefined, en).title).toBe("zsh command failed");
    expect(getToolPresentation(item, undefined, en, "Run false").title).toBe("zsh: Run false (failed)");
    // Only shell tools take one.
    expect(getToolPresentation(tool("read", { path: "a.txt" }, "x"), undefined, en, "ignored").title).toBe("Read a.txt");
  });
});

describe("tool titles that name their subject", () => {
  const en = (zh: string, en: string, params?: Record<string, string | number>) => translate("en-US", zh, en, params);

  it("names the file a call read or wrote, and the lines its change added and removed", () => {
    const edited = tool("edit", { path: "src/components/App.tsx", find: "a", replace: "b" }, "ok", {
      diff: "--- src/components/App.tsx\n+++ src/components/App.tsx\n@@ -1,2 +1,3 @@\n-a\n+b\n+c\n d\n"
    });
    expect(getToolPresentation(edited)).toMatchObject({
      title: "已编辑：App.tsx",
      subject: { before: "已编辑：", text: "App.tsx", after: "", path: "src/components/App.tsx" },
      counts: { additions: 2, deletions: 1 }
    });
    expect(getToolPresentation(edited, undefined, en).subject).toMatchObject({ before: "Edited ", text: "App.tsx" });

    const read = tool("read", { path: "C:\\work\\notes.md", start_line: 40 }, "line");
    expect(getToolPresentation(read)).toMatchObject({
      title: "已读取：notes.md",
      subject: { text: "notes.md", path: "C:\\work\\notes.md", line: 40 }
    });
    expect(getToolPresentation(read).counts).toBeUndefined();
    // Read from the top, the file opens at its top.
    expect(getToolPresentation(tool("read", { path: "a.txt" }, "x")).subject?.line).toBeUndefined();

    const written = tool("write", { path: "README.md", content: "x" }, "ok", {
      diff: "--- README.md\n+++ README.md\n@@ -1 +1 @@\n-old\n+x\n"
    });
    expect(getToolPresentation(written, undefined, en).title).toBe("Wrote README.md");

    // Still running or failed, the phrase stays: there is nothing yet to name.
    const running = { ...edited, streaming: true, streamStatus: "running" as const };
    expect(getToolPresentation(running).title).toBe("正在编辑文件");
    expect(getToolPresentation(running).subject).toBeUndefined();
  });

  it("names what a search looked for, set as code, and how much it found", () => {
    const found = tool("find", { query: "*.tsx", path: "src" }, [
      "src/App.tsx",
      "src/main.tsx",
      "dist/x.tsx (ignored)",
      "(Showing 3 of 9 matches. Narrow the pattern or path to see the rest.)",
      "(1 of the matches are in paths Git ignores.)"
    ].join("\n"));
    expect(getToolPresentation(found)).toMatchObject({
      title: "已查找：*.tsx",
      subject: { text: "*.tsx", code: true },
      note: "3+ 个结果"
    });
    expect(getToolPresentation(tool("find", { query: "*.rs" }, "No matching files")).note).toBe("0 个结果");

    const searched = tool("grep", { pattern: "TODO|FIXME" }, [
      "src/a.ts:3:// TODO one",
      "src/b.ts:10:// FIXME two",
      "[skipped] src/c.bin: unreadable"
    ].join("\n"));
    expect(getToolPresentation(searched, undefined, en)).toMatchObject({
      title: "Searched for TODO|FIXME",
      subject: { text: "TODO|FIXME", code: true },
      note: "2 matches"
    });
    expect(getToolPresentation(tool("grep", { pattern: "x" }, "src/a.ts:1:x\n… more matches follow.")).note)
      .toBe("1+ 处匹配");
  });
});

describe("failed tool titles", () => {
  const en = (zh: string, en: string, params?: Record<string, string | number>) => translate("en-US", zh, en, params);

  it("says why after what failed: the error's own line until the local model's reason replaces it", () => {
    const edit = tool("edit", { path: "src/App.tsx" }, "The exact text to replace was not found", { success: false });
    expect(getToolPresentation(edit).title).toBe("编辑文件失败：The exact text to replace was not found");
    expect(getToolPresentation(edit, undefined, en).title).toBe("Failed to edit file: The exact text to replace was not found");
    expect(getToolPresentation(edit, undefined, undefined, undefined, "找不到要替换的文本").title)
      .toBe("编辑文件失败：找不到要替换的文本");
    // What failed, without why: the body prints the whole error under it.
    expect(getToolPresentation(edit).failure).toBe("编辑文件失败");
    // A failed edit names no file and counts no lines.
    expect(getToolPresentation(edit).subject).toBeUndefined();
  });

  it("reads past a command's exit code and its colours to the line that explains it", () => {
    const failed = tool("bash", { command: "pnpm i" }, "Exit code 127\n\u001b[31mbash: pnpm: command not found\u001b[0m\nmore", {
      success: false
    });
    expect(getToolPresentation(failed).title).toBe("Bash 命令失败：bash: pnpm: command not found");
    // The reason outranks the command's description.
    expect(getToolPresentation(failed, undefined, undefined, "安装依赖", "没有安装 pnpm").title).toBe("Bash 命令失败：没有安装 pnpm");
    expect(errorExcerpt("Exit code 1")).toBe("Exit code 1");
    expect(errorExcerpt("  \n ")).toBeUndefined();
    expect(errorExcerpt("x".repeat(500))?.length).toBe(240);
  });

  it("keeps the phrase alone when the call returned nothing to quote", () => {
    const silent = tool("zsh", { command: "false" }, "", { success: false });
    expect(getToolPresentation(silent).title).toBe("zsh 命令失败");
    expect(getToolPresentation(silent, undefined, en, "Run false").title).toBe("zsh: Run false (failed)");
  });
});

describe("tool view registry", () => {
  it("shows actual thumbnails for image-producing read results", async () => {
    const image = await previewPixel("diagram.png");
    const item = tool("read", { path: "diagram.png" }, "已读取图片 diagram.png（image/png，1×1，68 字节）");
    item.result.images = [image];

    render(<ToolDetailRenderer item={item} />);

    expect(getToolPresentation(item).stat).toBe("1×1");
    expect(await screen.findByRole("img", { name: "diagram.png" })).toHaveAttribute(
      "src",
      PREVIEW_PIXEL_DATA_URL
    );
    expect(screen.queryByText("文件内容为空")).not.toBeInTheDocument();
    // The receipt line the model reads must not be repeated under the thumbnail.
    expect(screen.queryByText(/已读取图片/)).not.toBeInTheDocument();
  });

  it("explicitly registers the complete catalog and every legacy agent event", () => {
    for (const descriptor of toolCatalog) {
      expect(TOOL_VIEW_REGISTRY).toHaveProperty(descriptor.name);
    }
    // `structured_output` is deliberately NOT catalog-visible, so the loop
    // above cannot reach it; naming it here is what turns a forgotten
    // registry entry from a silently mis-rendered timeline into a failure.
    // The retired agent tools are not in the catalog either, and saved
    // conversations still carry their cards.
    for (const name of [
      "subagent",
      "subagent_update",
      "structured_output",
      "subagent_activity",
      "update",
      "agent_send",
      "send_message",
      "followup_task"
    ]) {
      expect(TOOL_VIEW_REGISTRY).toHaveProperty(name);
      expect(toolSurfaceForName(name)).toBe("group");
    }
    expect(toolSurfaceForName("ask_user")).toBe("question");
    // A running workflow owns a progress card of its own; a workflow step is an
    // ordinary agent-run row, so the two workflow tools deliberately disagree.
    expect(toolSurfaceForName("workflow")).toBe("workflow");
    expect(toolSurfaceForName("workflow_step")).toBe("group");
    for (const name of [
      "agent_spawn",
      "agent_send",
      "send_message",
      "followup_task",
      "task_wait",
      "task_list"
    ]) {
      expect(toolSurfaceForName(name)).toBe("group");
    }
    // The plan has a page of its own, so its tools stay ordinary rows and the
    // `plan` row states which half of the tool ran.
    for (const name of ["plan", "exit_plan_mode"]) {
      expect(toolSurfaceForName(name)).toBe("group");
    }
    expect(getToolPresentation(tool("plan", { action: "read" }, "{}")).title).toBe("已读取计划");
    expect(getToolPresentation(tool("plan", { action: "write", content: "# x" }, "{}")).title).toBe("已更新计划");
    // A saved `send_message` carries two contracts under one wire name. The
    // child form has no `target` because its one recipient was the main agent,
    // so it is a note travelling upward rather than a row that opens a child
    // transcript.
    const downward = getToolPresentation(tool("send_message", { target: "helper", message: "补充" }, "已排队"));
    expect(downward.family).toBe("agent-run");
    expect(downward.title).toBe("消息已加入子代理队列");
    expect(downward.target).toBe("helper");
    const upward = getToolPresentation(tool("send_message", { message: "已定位到根因" }, "已排队"));
    expect(upward.family).toBe("agent-note");
    expect(upward.title).toBe("向主代理发送了消息");
    expect(upward.target).toBe("已定位到根因");
    expect(toolSurfaceForName("future_tool")).toBe("group");
  });


  it("builds natural presentations and a semantic multi-tool summary", () => {
    const created = tool("write", { path: "src/new.ts", content: "new\n" }, "已写入", {
      diff: "--- /dev/null\n+++ src/new.ts\n@@ -0,0 +1 @@\n+new\n"
    });
    const shell = tool("powershell", { command: "npm test" }, "ok");
    const browser = tool("preview_click", { selector: "button.primary" }, "Successfully clicked: button.primary");

    expect(getToolPresentation(created)).toMatchObject({
      title: "已创建：new.ts",
      counts: { additions: 1, deletions: 0 },
      target: "src/new.ts",
      stat: "+1 −0",
      family: "diff"
    });
    // The heading counts buckets, not calls, and states them in importance
    // order regardless of the order the block ran them in.
    expect(summarizeBlockKinds([created, shell, browser].map(toolSummaryKind)))
      .toBe("运行了 1 个命令，1 次文件操作，执行了 1 次浏览器操作");
    // Agent calls are ordinary rows now, so they count toward the block
    // summary instead of leaving it claiming there is nothing to show.
    expect(summarizeBlockKinds([
      tool("agent_spawn", { label: "审查" }, "已派生"),
      tool("read", { path: "src/a.ts" }, "内容")
    ].map(toolSummaryKind))).toBe("调用了 1 个子代理，读取了 1 个文件");
    // Reasoning and hooks share the block, so they share its vocabulary even
    // though neither is a tool call with a registry entry.
    expect(summarizeBlockKinds(["reasoning", "hook", "fork", "skill"]))
      .toBe("请求了 1 次会话分叉，触发了 1 个 hook，读取了 1 个技能，思考了 1 次");
    // Repeats collapse into one counted clause per bucket.
    expect(summarizeBlockKinds(["reads", "files", "reads", "commands", "mcp"]))
      .toBe("运行了 1 个命令，调用了 1 个 MCP 工具，读取了 2 个文件，检查了 1 次文件与目录");
    // A lone bucket is still counted; there is no special single-item phrasing.
    expect(summarizeBlockKinds(["files"])).toBe("检查了 1 次文件与目录");
    // An empty block never reaches a heading, so the summary makes no claim.
    expect(summarizeBlockKinds([])).toBe("");
  });

  it("buckets every row by what it changed, not by which family renders it", () => {
    const buckets: ReadonlyArray<readonly [string, ToolContext["input"], string]> = [
      ["powershell", { command: "npm test" }, "commands"],
      ["bash", { command: "ls" }, "commands"],
      ["write", { path: "a.ts", content: "x" }, "fileChanges"],
      ["edit", { path: "a.ts", find: "x", replace: "y" }, "fileChanges"],
      // A workflow is subagent work under another name, so it counts as agents.
      ["workflow", { name: "release" }, "agents"],
      ["workflow_step", { label: "build" }, "agents"],
      ["agent_spawn", { name: "reviewer", prompt: "审查" }, "agents"],
      // A retired child → main message, as saved conversations hold it.
      ["send_message", { message: "已定位" }, "agents"],
      // `fork` and `skill` each own a bucket rather than falling into `other`.
      ["fork", { prompt: "继续" }, "fork"],
      ["skill", { name: "pdf" }, "skill"],
      ["read_project_memory", { name: "build" }, "memory"],
      ["grep", { pattern: "TODO" }, "search"],
      ["web_search", { query: "tauri" }, "search"],
      // Reading a file is its own bucket; listing and finding stay `files`.
      ["read", { path: "a.ts" }, "reads"],
      ["ls", { path: "." }, "files"],
      ["find", { query: "*.ts" }, "files"],
      ["preview_click", { selector: "button" }, "browser"],
      // Unregistered tools, and registered ones that changed nothing outside
      // the conversation, share the catch-all bucket.
      ["plan", { action: "read" }, "other"],
      ["ask_user", { questions: [] }, "other"],
      ["future_tool", { value: 1 }, "other"],
      [MCP_WIRE_NAME, { selector: "button" }, "mcp"]
    ];
    for (const [name, input, kind] of buckets) {
      expect([name, toolSummaryKind(tool(name, input, "ok"))]).toEqual([name, kind]);
    }
  });

  it("names MCP calls by the server and tool the user installed", () => {
    const call = tool(MCP_WIRE_NAME, { selector: "button.primary" }, "clicked");

    // The descriptor's label is the host's own words for both halves.
    expect(mcpToolNaming(call, mcpDescriptor("MCP Playwright / browser_click")))
      .toEqual({ server: "Playwright", tool: "browser_click" });
    // A trailing parenthetical is the manual-confirmation notice; it belongs in
    // the approval dock, never in the row's name.
    expect(mcpToolNaming(call, mcpDescriptor("MCP Playwright / browser_click (需要手动确认)")))
      .toEqual({ server: "Playwright", tool: "browser_click" });
    // Without a descriptor — an archived call whose server has been removed —
    // the wire name still reads, minus the server slug's trailing digest.
    expect(mcpToolNaming(call)).toEqual({ server: "playwright", tool: "browser_click" });
    // A name too short to split names only the tool, and claims no server.
    expect(mcpToolNaming(tool("mcp__orphan", {}, "ok"))).toEqual({ tool: "mcp__orphan" });

    expect(isMcpToolName(MCP_WIRE_NAME)).toBe(true);
    expect(isMcpToolName("mcp__orphan")).toBe(true);
    for (const ordinary of ["read", "bash", "mcp_legacy", "preview_click", "not_mcp__tool", ""]) {
      expect([ordinary, isMcpToolName(ordinary)]).toEqual([ordinary, false]);
    }

    // A row is one line, so it shows the tool's name — the wire name for
    // built-ins, and for MCP the tool's own half of the label.
    expect(toolRowName(tool("read", { path: "a.ts" }, "内容"))).toBe("read");
    expect(toolRowName(tool("future_tool", {}, "ok"))).toBe("future_tool");
    expect(toolRowName(call)).toBe("browser_click");
    expect(toolRowName(call, mcpDescriptor("MCP Playwright / browser_click (需要手动确认)")))
      .toBe("browser_click");
  });

  it("presents an MCP call as a plugged-in tool rather than an unknown one", () => {
    const call = tool(MCP_WIRE_NAME, { selector: "button.primary", timeout: 500 }, "clicked");

    expect(getToolPresentation(call, mcpDescriptor("MCP Playwright / browser_click"))).toMatchObject({
      surface: "group",
      family: "raw",
      icon: Plug,
      title: "调用了 MCP 工具 browser_click",
      // The first scalar argument is the one the tool's author put first.
      target: "button.primary",
      stat: "12 ms"
    });
    // The wire-name fallback keeps the row readable with no descriptor at all.
    expect(getToolPresentation(call)).toMatchObject({
      icon: Plug,
      title: "调用了 MCP 工具 browser_click",
      target: "button.primary"
    });
    // With no arguments to show, the row names the server it reached.
    expect(getToolPresentation(tool(MCP_WIRE_NAME, {}, "ok")).target).toBe("playwright");

    const running = { ...call, streaming: true, streamStatus: "running" as const };
    const failed = { ...call, result: { ...call.result, success: false } };
    expect(getToolPresentation(running).title).toBe("正在调用 MCP 工具 browser_click");
    expect(getToolPresentation(failed).title).toBe(`MCP 工具 browser_click 调用失败：${call.result.output}`);
    expect(getToolPresentation({ ...failed, result: { ...failed.result, output: "" } }).title).toBe("MCP 工具 browser_click 调用失败");

    const en = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) => (
      translate("en-US", zhCn, enUs, parameters)
    );
    const notice = mcpDescriptor("MCP Playwright / browser_click (requires manual confirmation)");
    expect(getToolPresentation(call, notice, en).title).toBe("Called MCP tool browser_click");
    expect(getToolPresentation(running, notice, en).title).toBe("Calling MCP tool browser_click");
    expect(getToolPresentation(failed, notice, en).title).toBe(`MCP tool browser_click failed: ${call.result.output}`);

    // Only `mcp__` names take that branch; other unregistered tools keep the
    // generic wrench so a plug never claims a server that does not exist.
    expect(getToolPresentation(tool("future_tool", {}, "ok"), {
      ...mcpDescriptor("Future tool", "future_tool"),
      category: "shell"
    })).toMatchObject({ icon: Wrench, title: "使用了 Future tool" });
  });

  it("localizes built-in titles, statistics, and summaries without translating tool payloads", () => {
    const t = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) => (
      translate("en-US", zhCn, enUs, parameters)
    );
    const listing = tool("ls", { path: "用户目录" }, "a.txt\nb.txt\n");
    expect(getToolPresentation(listing, undefined, t)).toMatchObject({
      title: "Viewed directory",
      target: "用户目录",
      stat: "2 items"
    });
    expect(summarizeBlockKinds([toolSummaryKind(listing)], t))
      .toBe("Checked files and directories once");
    expect(summarizeBlockKinds(
      [listing, tool("powershell", { command: "Write-Output 完成" }, "完成")].map(toolSummaryKind),
      t
    )).toBe("Ran 1 command, Checked files and directories once");
    // Every bucket has a singular English form; the plural is not a fallback.
    expect(summarizeBlockKinds(
      ["fileChanges", "agents", "mcp", "fork", "hook", "skill", "reads", "reasoning"],
      t
    )).toBe([
      "1 file change",
      "Called 1 subagent",
      "Called 1 MCP tool",
      "Requested 1 conversation fork",
      "Triggered 1 hook",
      "Loaded 1 skill",
      "Read 1 file",
      "Thought once"
    ].join(", "));
    expect(summarizeBlockKinds(["reads", "reads", "fileChanges", "fileChanges"], t))
      .toBe("2 file changes, Read 2 files");
    // The same block in the other locale joins its clauses with a full-width
    // comma, and states the identical buckets in the identical order.
    expect(summarizeBlockKinds(["reads", "reads", "fileChanges", "fileChanges"]))
      .toBe("2 次文件操作，读取了 2 个文件");
  });
});

describe("ordinary tool detail renderers", () => {
  it("renders ls/find lists and grep matches as navigable structured rows", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool("ls", { path: "." }, "src/\nsrc/App.tsx\n")} />);
    const files = screen.getByRole("list", { name: "文件结果" });
    expect(within(files).getByText("src/")).toBeInTheDocument();
    expect(within(files).getByText("src/App.tsx")).toBeInTheDocument();

    rerender(<ToolDetailRenderer item={tool("grep", { pattern: "TODO" }, "src/a.ts:12:// TODO: fix\nsrc/b.ts:7:TODO\n")} />);
    const matches = screen.getByRole("list", { name: "搜索结果" });
    expect(within(matches).getByText("src/a.ts")).toBeInTheDocument();
    expect(within(matches).getByLabelText("第 12 行")).toBeInTheDocument();
    expect(within(matches).getByText("// TODO: fix")).toBeInTheDocument();
  });

  it("renders read output on the diff base with source line numbers", () => {
    render(<ToolDetailRenderer item={tool("read", { path: "src/a.ts", start_line: 8 }, "first\nsecond")} />);
    const preview = screen.getByRole("region", { name: "src/a.ts 内容" });
    expect(preview.classList.contains("diff-output")).toBe(true);
    expect(preview.querySelectorAll(".diff-output__line")).toHaveLength(2);
    expect(preview.querySelector(".diff-output__line-number")).toHaveTextContent("8");
    expect(within(preview).getByText("second")).toBeInTheDocument();
  });

  it("prefers DiffOutput for successful write/edit calls", () => {
    const item = tool("edit", { path: "src/a.ts", find: "old", replace: "new" }, "完成替换", {
      diff: "--- src/a.ts\n+++ src/a.ts\n@@ -1 +1 @@\n-old\n+new\n"
    });
    render(<ToolDetailRenderer item={item} />);
    expect(screen.getByRole("region", { name: "src/a.ts 文件差异" })).toBeInTheDocument();
    expect(screen.getByLabelText("新增 1 行，删除 1 行")).toBeInTheDocument();
  });

  it("renders shell input and terminal output without interpreting markup", () => {
    const { container } = render(<ToolDetailRenderer item={tool("bash", { command: "printf '<tag>'" }, "<tag>\n[stderr]\nwarning")} />);
    expect(screen.getByText("printf '<tag>'")).toBeInTheDocument();
    expect(container.querySelector(".tool-renderer__terminal-output")).toHaveTextContent("[stderr]");
    expect(document.querySelector("tag")).toBeNull();
  });

  it("lays a curated call out as the arguments worth reading and then its result", () => {
    // `web_search` normally draws its own card, but only when its result is the
    // envelope the host emits. Output that is not that envelope — a rerun with a
    // hand-written result, an older transcript — falls back to this shared card
    // rather than rendering an empty one.
    const { container } = render(<ToolDetailRenderer item={tool(
      "web_search",
      { query: "tauri", maxResults: 3 },
      "1. Tauri — https://tauri.app"
    )} />);

    // `maxResults` is plumbing nobody opened the card to read. The result is the
    // one row every card has, so it is the one row that carries no name.
    const rows = Array.from(container.querySelectorAll(".tool-kv__row"));
    expect(rows.map((row) => row.querySelector(".tool-kv__key")?.textContent))
      .toEqual(["query", undefined]);
    expect(rows.map((row) => row.querySelector(".tool-kv__value")?.textContent))
      .toEqual(["tauri", "1. Tauri — https://tauri.app"]);
    expect(rows[1]).toHaveClass("tool-kv__row--result");
  });

  /**
   * A search card leads with the sites it read. The chips are the part a reader
   * can check: the prose under them is a model's summary for a native backend
   * and the providers' own snippets for a catalog one, but the domains say where
   * any of it came from.
   */
  it("draws one chip per site a native search consulted, and counts them in the row title", () => {
    const item = tool(
      "web_search",
      { query: "provider-executed web search" },
      JSON.stringify({
        findings: "Anthropic seals search result text in encrypted_content.",
        sources: [
          { url: "https://docs.anthropic.com/en/docs/web-search-tool", title: "Web search tool" },
          { url: "https://www.docs.anthropic.com/en/other", title: "Another page" },
          { url: "https://platform.openai.com/docs/guides/tools-web-search", title: "Web search" }
        ],
        untrustedWebContent: true
      })
    );
    const { container } = render(<ToolDetailRenderer item={item} />);

    const chips = Array.from(container.querySelectorAll(".tool-renderer__source"));
    // Two pages of one site are one chip: the row answers "where did this come
    // from", and `www.` never distinguishes two sources.
    expect(chips.map((chip) => chip.querySelector(".tool-renderer__source-host")?.textContent))
      .toEqual(["docs.anthropic.com", "platform.openai.com"]);
    expect(chips[0]).toHaveAttribute("href", "https://docs.anthropic.com/en/docs/web-search-tool");
    expect(chips[0].getAttribute("title")).toContain("Web search tool");
    expect(container.querySelector(".tool-renderer__findings"))
      .toHaveTextContent("Anthropic seals search result text");

    // The collapsed row says how many sites, not "completed a web search".
    const en = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) => (
      translate("en-US", zhCn, enUs, parameters)
    );
    expect(getToolPresentation(item).title).toBe("已搜索 2 个网站");
    expect(getToolPresentation(item, undefined, en).title).toBe("Searched 2 sites");
  });

  it("builds the same chips from a catalog provider's result array", () => {
    // The two backends answer in different shapes and both are load-bearing:
    // a provider-native search has no host-readable page text, so it returns
    // `sources`; a catalog provider returns `results` with real content.
    const { container } = render(<ToolDetailRenderer item={tool(
      "web_search",
      { query: "tauri" },
      JSON.stringify({
        results: [
          { id: "ab-1", title: "Tauri", url: "https://tauri.app/start", content: "Build smaller apps." }
        ],
        untrustedWebContent: true
      })
    )} />);

    expect(container.querySelector(".tool-renderer__source-host")).toHaveTextContent("tauri.app");
    expect(container.querySelector(".tool-renderer__result-list strong")).toHaveTextContent("Tauri");
    expect(container.querySelector(".tool-renderer__result-list p")).toHaveTextContent("Build smaller apps.");
  });

  it("stands in a lettered placeholder while a site has no icon", () => {
    // The renderer cannot fetch a favicon itself — the CSP allows `img-src
    // 'self' data:` and nothing outbound — so the host supplies one. With no
    // host (browser preview, tests) every chip keeps its letter.
    const { container } = render(<ToolDetailRenderer item={tool(
      "web_search",
      { query: "x" },
      JSON.stringify({ findings: "f", sources: [{ url: "https://example.com/a" }] })
    )} />);

    expect(container.querySelector(".tool-renderer__source-letter")).toHaveTextContent("E");
    expect(container.querySelector(".tool-renderer__source img")).toBeNull();
  });

  it("keeps fetching on the ordinary card, because its URLs are already in the call", () => {
    expect(TOOL_VIEW_REGISTRY.web_fetch.family).toBe("raw");
    expect(TOOL_VIEW_REGISTRY.web_search.family).toBe("web-search");
  });

  it("shows every argument of a tool whose arguments nobody has curated", () => {
    // An uncurated argument is still the reader's only account of the call, so
    // an unknown tool shows all of them rather than none.
    const { container } = render(
      <ToolDetailRenderer item={tool("future_tool", { value: 1, mode: "fast" }, "ok")} />
    );
    expect([...kvRows(container).keys()]).toEqual(["value", "mode", ""]);
  });
});

describe("preview tool detail families", () => {
  it("renders the accessibility tree a snapshot answered with, as text", () => {
    const tree = ["- document", "  - button \"Continue\" [uid=e1]", "  - link \"Docs\" [uid=e2]"].join("\n");
    render(<ToolDetailRenderer item={tool("preview_snapshot", {}, tree)} />);
    const snapshot = screen.getByLabelText("页面快照");
    expect(snapshot).toHaveTextContent(/button "Continue" \[uid=e1\]/);
    expect(getToolPresentation(tool("preview_snapshot", {}, tree))).toMatchObject({
      family: "browser-snapshot",
      title: "读取了页面快照",
      stat: "3 个节点"
    });
  });

  it("renders the evaluated expression beside the value the page returned", () => {
    const { container, rerender } = render(<ToolDetailRenderer item={tool("preview_eval", { expression: "document.title" }, "\"Example\"")} />);
    expect(kvRows(container).get("JavaScript")).toBe("document.title");
    expect(kvRows(container).get("")).toBe('"Example"');

    // `undefined` is a value the tool words itself, not an empty result.
    rerender(<ToolDetailRenderer item={tool("preview_eval", { expression: "void 0" }, "undefined")} />);
    expect(kvRows(container).get("")).toBe("undefined");
  });

  it("renders screenshot dimensions, and thumbnails alone when pixels came back", async () => {
    const metadata = JSON.stringify({ width: 1280, height: 720 });
    const { rerender } = render(<ToolDetailRenderer item={tool("preview_screenshot", {}, metadata)} />);
    expect(screen.getByText("1280×720")).toBeInTheDocument();
    expect(getToolPresentation(tool("preview_screenshot", {}, metadata)).stat).toBe("1280×720");

    const image = await previewPixel("preview-20260714T000000.000Z");
    const withPixels = tool("preview_screenshot", {}, metadata);
    withPixels.result.images = [image];
    rerender(<ToolDetailRenderer item={withPixels} />);
    expect(await screen.findByRole("img", { name: "preview-20260714T000000.000Z" }))
      .toHaveAttribute("src", PREVIEW_PIXEL_DATA_URL);
    // The thumbnail is the result; its metadata only repeats what the image shows.
    expect(screen.queryByText("1280×720")).not.toBeInTheDocument();
  });

  it("splits console entries out of the tool's own line format and lifts its footer", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool(
      "preview_console_logs",
      { level: "all" },
      "[error] boom\n[log] ready\n\n(Showing last 2 of 9 entries. Use 'lines' parameter (max 200) to see more.)"
    )} />);
    const entries = screen.getByRole("list", { name: "Console 日志" });
    expect(within(entries).getByText("boom")).toBeInTheDocument();
    expect(within(entries).getByText("ready")).toBeInTheDocument();
    expect(within(entries).queryByText(/Showing last/)).not.toBeInTheDocument();
    expect(screen.getByText(/Showing last 2 of 9 entries/)).toBeInTheDocument();

    // The tool's own empty answer is not one entry.
    rerender(<ToolDetailRenderer item={tool("preview_console_logs", {}, "No console logs.")} />);
    expect(screen.getByText("没有 Console 日志")).toBeInTheDocument();
    expect(getToolPresentation(tool("preview_console_logs", {}, "No console logs.")).stat).toBe("0 条");
  });

  it("splits the network ledger but leaves a fetched response body as text", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool(
      "preview_network",
      { filter: "all" },
      "[req-1] GET https://example.com/api → 200 OK\n[req-2] POST https://example.com/save [FAILED: net::ERR_ABORTED]"
    )} />);
    const requests = screen.getByRole("list", { name: "网络请求" });
    expect(within(requests).getByText("https://example.com/api")).toBeInTheDocument();
    expect(within(requests).getByText("200 OK")).toBeInTheDocument();
    expect(within(requests).getByText("[FAILED: net::ERR_ABORTED]")).toBeInTheDocument();

    rerender(<ToolDetailRenderer item={tool(
      "preview_network",
      { requestId: "req-1" },
      "{\"ok\":true}"
    )} />);
    expect(screen.queryByRole("list", { name: "网络请求" })).not.toBeInTheDocument();
    expect(screen.getByText('{"ok":true}')).toBeInTheDocument();
  });

  it("re-indents the compact JSON that inspect and list answer with", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool(
      "preview_inspect",
      { selector: ".button" },
      '{"tagName":"BUTTON","id":"submit"}'
    )} />);
    expect(screen.getByText(/"tagName": "BUTTON"/)).toBeInTheDocument();

    // Element-not-found is prose, not JSON, and stays readable.
    rerender(<ToolDetailRenderer item={tool("preview_inspect", { selector: ".gone" }, "Element not found: .gone")} />);
    expect(screen.getByText("Element not found: .gone")).toBeInTheDocument();

    const servers = JSON.stringify([{ serverId: "srv-1", name: "dev", port: 5173, status: "running" }]);
    rerender(<ToolDetailRenderer item={tool("preview_list", {}, servers)} />);
    expect(screen.getByText(/"name": "dev"/)).toBeInTheDocument();
    expect(getToolPresentation(tool("preview_list", {}, servers)).stat).toBe("1 个服务器");
  });

  it("reports where the page ended up after Mework's own two preview tools", () => {
    const { container } = render(<ToolDetailRenderer item={tool("preview_dialog", { accept: true }, JSON.stringify({
      answered: true,
      page: { url: "http://localhost:5173/", title: "Dev", loading: false },
      newConsoleErrors: ["TypeError: x is not a function"],
      notices: ["The page opened a modal state that blocks it."],
      snapshot: { tree: "- button \"OK\" [uid=e1]", truncated: false }
    }))} />);
    const rows = kvRows(container);
    expect(rows.get("URL")).toBe("http://localhost:5173/");
    expect(rows.get("标题")).toBe("Dev");
    expect(rows.get("提示")).toBe("The page opened a modal state that blocks it.");
    expect(rows.get("新增 Console 错误")).toBe("TypeError: x is not a function");
    expect(rows.get("")).toMatch(/button "OK"/);
    expect(getToolPresentation(tool("preview_dialog", { accept: false }, "{}")).stat).toBe("取消");
  });

  it("keeps the failure message even when the result carries images", async () => {
    const image = await previewPixel("page.png");
    const item = tool("read", { path: "diagram.png" }, "图片超过 5 MiB 限制", { success: false });
    item.result.images = [image];

    render(<ToolDetailRenderer item={item} />);

    expect(screen.getByRole("alert")).toHaveTextContent("图片超过 5 MiB 限制");
  });

  it("uses a safe text fallback for malformed preview JSON", () => {
    render(<ToolDetailRenderer item={tool("preview_dialog", {}, "{not-json")} />);
    expect(screen.getByText("结果不是有效 JSON")).toBeInTheDocument();
    expect(screen.getByText("{not-json")).toBeInTheDocument();
  });

  it("renders the prose the thirteen copied tools answer with, unchanged", () => {
    // `preview_click` / `preview_fill` / `preview_resize` / `preview_start` return the source's own
    // sentences. Re-parsing them would be inventing structure the host never sent.
    const { rerender } = render(<ToolDetailRenderer item={tool(
      "preview_click",
      { selector: "button.primary", doubleClick: true },
      "Successfully double-clicked: button.primary"
    )} />);
    expect(screen.getByText("Successfully double-clicked: button.primary")).toBeInTheDocument();
    expect(getToolPresentation(tool("preview_click", { selector: "button.primary", doubleClick: true }, "ok")))
      .toMatchObject({ family: "raw", title: "点击了页面元素", target: "button.primary", stat: "双击" });

    rerender(<ToolDetailRenderer item={tool(
      "preview_start",
      { name: "dev" },
      "{\n  \"name\": \"dev\"\n}\nServer started successfully on port 5173."
    )} />);
    expect(screen.getByText(/Server started successfully on port 5173\./)).toBeInTheDocument();
  });
});

describe("memory tool presentation", () => {
  const titleCases = [
    ["read_global_memory", "读取了全局记忆", "正在读取全局记忆", "读取全局记忆失败",
      "Read global memory", "Reading global memory", "Failed to read global memory"],
    ["read_project_memory", "读取了项目记忆", "正在读取项目记忆", "读取项目记忆失败",
      "Read project memory", "Reading project memory", "Failed to read project memory"],
    ["create_global_memory", "创建了全局记忆", "正在创建全局记忆", "创建全局记忆失败",
      "Created global memory", "Creating global memory", "Failed to create global memory"],
    ["create_project_memory", "创建了项目记忆", "正在创建项目记忆", "创建项目记忆失败",
      "Created project memory", "Creating project memory", "Failed to create project memory"],
    ["edit_global_memory", "编辑了全局记忆", "正在编辑全局记忆", "编辑全局记忆失败",
      "Edited global memory", "Editing global memory", "Failed to edit global memory"],
    ["edit_project_memory", "编辑了项目记忆", "正在编辑项目记忆", "编辑项目记忆失败",
      "Edited project memory", "Editing project memory", "Failed to edit project memory"]
  ] as const;

  it.each(titleCases)(
    "registers %s with localized done, running, and failed titles",
    (name, doneZh, runningZh, failedZh, doneEn, runningEn, failedEn) => {
      const item = tool(name, { name: "build" }, "记忆正文");
      const running = { ...item, streaming: true, streamStatus: "running" as const };
      // Nothing to quote, so the title is the registered phrase alone.
      const failed = { ...item, result: { ...item.result, success: false, output: "" } };
      expect(getToolPresentation(item).title).toBe(doneZh);
      expect(getToolPresentation(running).title).toBe(runningZh);
      expect(getToolPresentation(failed).title).toBe(failedZh);

      const en = (zhCn: string, enUs: string, parameters?: Record<string, string | number>) => (
        translate("en-US", zhCn, enUs, parameters)
      );
      expect(getToolPresentation(item, undefined, en).title).toBe(doneEn);
      expect(getToolPresentation(running, undefined, en).title).toBe(runningEn);
      expect(getToolPresentation(failed, undefined, en).title).toBe(failedEn);
    }
  );

  it("targets the document by the name the model asked for", () => {
    for (const name of [
      "read_global_memory",
      "read_project_memory",
      "create_global_memory",
      "create_project_memory",
      "edit_global_memory",
      "edit_project_memory"
    ]) {
      expect(getToolPresentation(tool(name, { name: "build-environment" }, "ok")).target)
        .toBe("build-environment");
    }
  });

  it("labels each tier and shows the document body a read returned", () => {
    const { container } = render(
      <ToolDetailRenderer item={tool("read_project_memory", { name: "build" }, "测试要用项目自带环境")} />
    );
    expect(container).toHaveTextContent("项目记忆");
    expect(container).not.toHaveTextContent("全局记忆");
    expect(container).toHaveTextContent("build");
    expect(container).toHaveTextContent("测试要用项目自带环境");
  });

  it("shows what a write recorded: index description, body and raw data", () => {
    const { container } = render(
      <ToolDetailRenderer
        item={tool(
          "create_global_memory",
          { name: "preferences", content: "回答一律用中文", description: "用户长期偏好" },
          "已在全局记忆中创建 preferences.md，并写入索引描述。"
        )}
      />
    );
    expect(container).toHaveTextContent("全局记忆");
    expect(container).toHaveTextContent("用户长期偏好");
    expect(container).toHaveTextContent("回答一律用中文");
    expect(container).toHaveTextContent("已在全局记忆中创建 preferences.md");
    expect(screen.getByText("原始数据")).toBeInTheDocument();
  });

  it("shows the passage an edit replaced and what replaced it", () => {
    const { container } = render(
      <ToolDetailRenderer
        item={tool(
          "edit_project_memory",
          { name: "build", old_text: "端口 3000", new_text: "端口 4000" },
          "已在项目记忆中编辑 build.md。"
        )}
      />
    );
    expect(container).toHaveTextContent("端口 3000");
    expect(container).toHaveTextContent("端口 4000");
  });

  it("surfaces a failed memory call with its reason", () => {
    const item = tool(
      "edit_project_memory",
      { name: "build", old_text: "3000", new_text: "4000", description: "端口" },
      "要替换的原文在记忆文档 build.md 中出现了 2 次；请提供唯一匹配的更长片段",
      { success: false }
    );

    render(<ToolDetailRenderer item={item} />);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("编辑项目记忆失败");
    expect(alert).toHaveTextContent("build");
    expect(alert).toHaveTextContent("出现了 2 次");
  });

  it("carries no model identity, scope selector, or version anywhere in the card", () => {
    // Memory belongs to a location, not to a model. The retired system showed
    // an owner model ID, a scope row and a CAS version; none of that exists.
    const { container } = render(
      <ToolDetailRenderer
        item={tool(
          "create_project_memory",
          { name: "notes", content: "正文", description: "描述" },
          "已在项目记忆中创建 notes.md，并写入索引描述。"
        )}
      />
    );
    for (const retired of ["模型", "kimi-k3", "版本", "v1", "自动加载预算"]) {
      expect(container).not.toHaveTextContent(retired);
    }
  });
});

describe("agent detail views", () => {
  const runRecord = {
    name: "reviewer",
    label: "接口审查",
    task: "审查接口",
    status: "completed" as const,
    contexts: [],
    updates: [{ content: "重复内容", createdAt: "2026-07-14T01:04:00Z" }]
  };

  it("shows the instruction under whichever input key the run tool declared", () => {
    // The key differs per tool — agent_spawn declares `prompt` while the legacy
    // `subagent` call declares `task` — so reading only one left most rows empty.
    for (const [toolName, input, expected] of [
      ["agent_spawn", { name: "p", prompt: "用 prompt 传的指令" }, "用 prompt 传的指令"],
      ["subagent", { label: "T", task: "用 task 传的指令" }, "用 task 传的指令"],
      ["agent_send", { agent: "p", message: "用 message 传的追加指令" }, "用 message 传的追加指令"]
    ] as const) {
      const { unmount } = render(<ToolDetailRenderer item={tool(toolName, input, "已受理")} />);
      expect(screen.getByText(expected)).toBeInTheDocument();
      unmount();
    }
  });

  it("merges the live and persisted update feeds into one deduplicated, time-ordered list", () => {
    // A call in the handoff window carries both feeds with the same content in
    // each; showing it twice claimed two updates where the child sent one.
    const item: ToolContext = {
      ...tool("agent_spawn", { name: "reviewer", prompt: "审查接口" }, "已完成"),
      streaming: true,
      streamStatus: "completed",
      live: {
        contexts: [],
        status: "running",
        updates: [
          { content: "较旧更新", createdAt: "2026-07-14T01:02:00Z" },
          { content: "重复内容", createdAt: "2026-07-14T01:03:00Z" }
        ]
      },
      subagent: runRecord
    };
    const { container } = render(<ToolDetailRenderer item={item} />);

    // One value holding exactly the two distinct updates, oldest first, states
    // both halves at once: nothing was shown twice and nothing was reordered.
    expect(kvRows(container).get("子代理更新")).toBe("较旧更新\n重复内容");
    expect(screen.getByText("接口审查")).toBeInTheDocument();
  });

  it("splits task_wait envelopes and counts them on the row", () => {
    const output = [
      "[reviewer · 已完成]",
      "12 项检查全部通过",
      "",
      "[tester · 进度更新]",
      "正在跑 e2e",
      "",
      "当前状态：reviewer 已完成、tester 运行中"
    ].join("\n");
    const item = tool("task_wait", { tasks: ["reviewer", "tester"] }, output);

    expect(getToolPresentation(item)).toMatchObject({ family: "agent-wait", stat: "收到 2 条" });
    const { container } = render(<ToolDetailRenderer item={item} />);
    const rows = kvRows(container);
    expect([...rows.keys()]).toEqual(["reviewer · 已完成", "tester · 进度更新", ""]);
    expect(rows.get("reviewer · 已完成")).toBe("12 项检查全部通过");
    expect(rows.get("")).toBe("当前状态：reviewer 已完成、tester 运行中");
  });

  it("keeps an unparsable or empty wait result readable instead of dropping it", () => {
    const opaque = tool("task_wait", {}, "provider returned an opaque wait error");
    expect(getToolPresentation(opaque).stat).toBeUndefined();
    const { unmount } = render(<ToolDetailRenderer item={opaque} />);
    expect(screen.getByText("provider returned an opaque wait error")).toBeInTheDocument();
    unmount();

    render(<ToolDetailRenderer item={tool("task_wait", {}, "")} />);
    expect(screen.getByText("等待已结束，没有可显示的更新")).toBeInTheDocument();
  });

  it("shows a one-shot agent message in full and keeps its receipt beside it", () => {
    render(<ToolDetailRenderer item={tool(
      "subagent_update",
      { message: "正在跑 e2e" },
      "状态已返回给主智能体"
    )} />);
    expect(getToolPresentation(tool("subagent_update", { message: "x" }, "ok")).family).toBe("agent-note");
    expect(screen.getByText("正在跑 e2e")).toBeInTheDocument();
    expect(screen.getByText("状态已返回给主智能体")).toBeInTheDocument();
  });
});

describe("shared safeguards", () => {
  it("renders failures prominently and only materializes raw data after its disclosure opens", async () => {
    const user = userEvent.setup();
    render(<ToolDetailRenderer item={tool("read", { path: "missing.ts" }, "文件不存在", { success: false })} />);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("读取文件失败");
    expect(alert).toHaveTextContent("文件不存在");
    const disclosure = screen.getByText("原始数据").closest("details");
    expect(disclosure).not.toHaveAttribute("open");
    expect(disclosure).not.toHaveTextContent("missing.ts");
    await user.click(screen.getByText("原始数据"));
    expect(disclosure).toHaveTextContent("missing.ts");
  });

  it("falls back to a raw renderer for unknown future tools", () => {
    const item = tool("future_tool", { value: 1 }, "future output");
    expect(getToolPresentation(item)).toMatchObject({ family: "raw", surface: "group", title: "使用了 future_tool" });
    render(<ToolDetailRenderer item={item} />);
    expect(screen.getByText("future output")).toBeInTheDocument();
  });
});
