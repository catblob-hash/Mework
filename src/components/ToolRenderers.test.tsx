import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { translate } from "../i18n";
import { prepareImageAttachment } from "../lib/runtime";
import { toolCatalog } from "../seed";
import type { ToolContext } from "../types";
import {
  getToolPresentation,
  summarizeToolGroup,
  TOOL_VIEW_REGISTRY,
  ToolDetailRenderer,
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
    for (const name of ["subagent", "subagent_update", "structured_output", "subagent_activity", "update"]) {
      expect(TOOL_VIEW_REGISTRY).toHaveProperty(name);
      expect(toolSurfaceForName(name)).toBe("group");
    }
    // Every task action is an ordinary row now — writes and reads alike —
    // so the surface no longer depends on the call, only the detail family does.
    expect(toolSurfaceForName("todo")).toBe("group");
    expect(getToolPresentation(tool("todo", { action: "create", subject: "x", description: "y" }, "{}")).family).toBe("persistent");
    expect(getToolPresentation(tool("todo", { action: "update", taskId: "task-1", status: "completed" }, "{}")).family).toBe("persistent");
    expect(getToolPresentation(tool("todo", { action: "get", taskId: "task-1" }, "{}")).family).toBe("raw");
    expect(getToolPresentation(tool("todo", { action: "list" }, "{}")).family).toBe("raw");
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
    expect(toolSurfaceForName("future_tool")).toBe("group");
  });


  it("builds natural presentations and a semantic multi-tool summary", () => {
    const created = tool("write", { path: "src/new.ts", content: "new\n" }, "已写入", {
      diff: "--- /dev/null\n+++ src/new.ts\n@@ -0,0 +1 @@\n+new\n"
    });
    const shell = tool("powershell", { command: "npm test" }, "ok");
    const browser = tool("playwright", { action: "click", ref: "e12" }, JSON.stringify({ element: { ref: "e12", tag: "button" } }));

    expect(getToolPresentation(created)).toMatchObject({
      title: "创建了文件",
      target: "src/new.ts",
      stat: "+1 −0",
      family: "diff"
    });
    const summary = summarizeToolGroup([created, shell, browser]);
    expect(summary).toContain("编辑了 1 个文件");
    expect(summary).toContain("运行了 1 个命令");
    expect(summary).toContain("1 次浏览器操作");
    // Agent and task-state calls are ordinary rows now, so they count toward
    // the block summary instead of leaving it claiming there is nothing to show.
    expect(summarizeToolGroup([
      tool(
        "todo",
        { action: "create", subject: "x", description: "y" },
        JSON.stringify({ task: { id: "task-1", subject: "x" } })
      ),
      tool("agent_spawn", { label: "审查" }, "已派生"),
      tool("read", { path: "src/a.ts" }, "内容")
    ])).toBe("检查了 1 次文件与目录，1 次子代理操作，1 次状态变更");
    expect(summarizeToolGroup([])).toBe("没有普通工具调用");
  });

  it("presents task-state reads as raw output and writes as structured state", () => {
    expect(getToolPresentation(tool(
      "todo",
      { action: "update", taskId: "task-1", status: "in_progress" },
      JSON.stringify({ success: true, taskId: "task-1", updatedFields: ["status"] })
    ))).toMatchObject({
      surface: "group",
      family: "persistent",
      title: "更新了任务",
      target: "task-1",
      stat: "进行中"
    });
    expect(getToolPresentation(tool(
      "todo",
      { action: "list" },
      JSON.stringify({ tasks: [
        { id: "task-1", subject: "实现", status: "pending", blockedBy: [] },
        { id: "task-2", subject: "测试", status: "completed", blockedBy: [] }
      ] })
    ))).toMatchObject({
      surface: "group",
      family: "raw",
      title: "列出了任务",
      stat: "2 项"
    });
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
    expect(summarizeToolGroup([listing], t)).toBe("Viewed directory 用户目录");
    expect(summarizeToolGroup([
      listing,
      tool("powershell", { command: "Write-Output 完成" }, "完成")
    ], t)).toBe("Ran 1 command, Checked files and directories once");
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

  it("renders read output with source line numbers", () => {
    render(<ToolDetailRenderer item={tool("read", { path: "src/a.ts", start_line: 8 }, "first\nsecond")} />);
    const preview = screen.getByRole("region", { name: "src/a.ts 内容" });
    expect(preview.querySelectorAll(".tool-renderer__code-line")).toHaveLength(2);
    expect(preview.querySelector(".tool-renderer__line-number")).toHaveTextContent("8");
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
});

describe("browser tool detail families", () => {
  it("renders navigation, snapshot and ordinary browser action results", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool("playwright", { action: "navigate", url: "https://example.com" }, JSON.stringify({
      open: true, url: "https://example.com", title: "Example", loading: false, viewport: { width: 900, height: 600 }
    }))} />);
    expect(screen.getByText("Example")).toBeInTheDocument();
    expect(screen.getByText("900×600")).toBeInTheDocument();

    rerender(<ToolDetailRenderer item={tool("playwright", { action: "snapshot" }, JSON.stringify({
      url: "https://example.com", tree: "- button \"Continue\" [ref=e1]", elements: [{ ref: "e1" }]
    }))} />);
    expect(screen.getByText(/button "Continue"/)).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();

    rerender(<ToolDetailRenderer item={tool("playwright", { action: "click", ref: "e1" }, JSON.stringify({
      input: "input-pipeline", element: { ref: "e1", tag: "button", name: "Continue" }
    }))} />);
    expect(screen.getByText("button · e1")).toBeInTheDocument();
    expect(screen.getByText("input-pipeline")).toBeInTheDocument();
  });

  it("renders evaluate values and screenshot metadata", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool("playwright", { action: "evaluate", script: "await getValue()" }, "42")} />);
    expect(screen.getByText("await getValue()")).toBeInTheDocument();
    expect(screen.getByLabelText("JavaScript 返回值")).toHaveTextContent("42");

    rerender(<ToolDetailRenderer item={tool("playwright", { action: "screenshot", path: "shots/page.png" }, JSON.stringify({
      path: "shots/page.png", bytes: 2048, width: 1280, height: 720, fullPage: true
    }))} />);
    expect(screen.getByText("shots/page.png")).toBeInTheDocument();
    expect(screen.getByText("1280×720")).toBeInTheDocument();
  });

  it("renders browser screenshot thumbnails while isolating a missing sidecar", async () => {
    const image = await previewPixel("page.png");
    const item = tool("playwright", { action: "screenshot", path: "shots/page.png" }, JSON.stringify({
      path: "shots/page.png",
      bytes: 2048,
      width: 1280,
      height: 720,
      fullPage: true
    }));
    item.result.images = [
      image,
      {
        ...image,
        id: "f".repeat(64),
        name: "missing-sidecar.png"
      }
    ];

    render(<ToolDetailRenderer item={item} />);

    expect(await screen.findByRole("img", { name: "page.png" }))
      .toHaveAttribute("src", PREVIEW_PIXEL_DATA_URL);
    const failureStatus = await screen.findByRole("status");
    expect(failureStatus).toHaveTextContent("missing-sidecar.png 加载失败");
    expect(failureStatus).toHaveAttribute("aria-live", "polite");
    expect(screen.getByRole("list", { name: "2 张图片" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "截取了网页：shots/page.png" })).toBeInTheDocument();
    // Thumbnails replace the detail grid entirely — no path, size or byte rows.
    expect(screen.queryByText("shots/page.png")).not.toBeInTheDocument();
    expect(screen.queryByText("1280×720")).not.toBeInTheDocument();
    expect(screen.queryByText("2048")).not.toBeInTheDocument();
  });

  it("keeps the screenshot detail grid when no image sidecar came back", () => {
    render(<ToolDetailRenderer item={tool("playwright", { action: "screenshot", path: "shots/page.png" }, JSON.stringify({
      path: "shots/page.png",
      bytes: 2048,
      width: 1280,
      height: 720,
      fullPage: true
    }))} />);

    expect(screen.getByText("shots/page.png")).toBeInTheDocument();
    expect(screen.getByText("1280×720")).toBeInTheDocument();
  });

  it("keeps the failure message even when the result carries images", async () => {
    const image = await previewPixel("page.png");
    const item = tool("read", { path: "diagram.png" }, "图片超过 5 MiB 限制", { success: false });
    item.result.images = [image];

    render(<ToolDetailRenderer item={item} />);

    expect(screen.getByRole("alert")).toHaveTextContent("图片超过 5 MiB 限制");
  });

  it("renders console, network and dialog collections", () => {
    const { rerender } = render(<ToolDetailRenderer item={tool("playwright", { action: "console" }, JSON.stringify({
      entries: [{ level: "error", text: "boom" }], returned: 1
    }))} />);
    expect(screen.getByRole("list", { name: "Console 日志" })).toHaveTextContent("boom");

    rerender(<ToolDetailRenderer item={tool("playwright", { action: "network" }, JSON.stringify({
      entries: [{ method: "GET", status: 200, url: "https://example.com/api" }]
    }))} />);
    expect(screen.getByRole("list", { name: "网络日志" })).toHaveTextContent("https://example.com/api");

    rerender(<ToolDetailRenderer item={tool("playwright", { action: "dialog" }, JSON.stringify({
      records: [{ type: "confirm", message: "Continue?", result: true }]
    }))} />);
    expect(screen.getByRole("list", { name: "页面对话框记录" })).toHaveTextContent("Continue?");
  });

  it("uses a safe text fallback for malformed browser JSON", () => {
    render(<ToolDetailRenderer item={tool("playwright", { action: "snapshot" }, "{not-json")} />);
    expect(screen.getByText("结果不是有效 JSON")).toBeInTheDocument();
    expect(screen.getByText("{not-json")).toBeInTheDocument();
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
      const failed = { ...item, result: { ...item.result, success: false } };
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

  it("shows the index description a write recorded", () => {
    const { container } = render(
      <ToolDetailRenderer
        item={tool(
          "create_global_memory",
          { name: "preferences", content: "正文", description: "用户长期偏好" },
          "已在全局记忆中创建 preferences.md，并写入索引描述。"
        )}
      />
    );
    expect(container).toHaveTextContent("全局记忆");
    expect(container).toHaveTextContent("用户长期偏好");
    expect(container).toHaveTextContent("已在全局记忆中创建 preferences.md");
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

describe("agent and task-state detail views", () => {
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

    const updates = [...container.querySelectorAll(".tool-renderer__agent-updates li")];
    expect(updates.map((row) => row.textContent)).toEqual(["较旧更新", "重复内容"]);
    expect(screen.getAllByText("重复内容")).toHaveLength(1);
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
    const rows = [...container.querySelectorAll(".tool-renderer__wait-list > li")];
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent("reviewer");
    expect(rows[0]).toHaveTextContent("已完成");
    expect(rows[0]).toHaveTextContent("12 项检查全部通过");
    expect(screen.getByText("当前状态：reviewer 已完成、tester 运行中")).toBeInTheDocument();
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

  it("localizes task status instead of leaking the wire enum into copy", () => {
    expect(getToolPresentation(tool("todo", { action: "update", taskId: "t", status: "deleted" }, "{}")).stat)
      .toBe("已删除");
    render(<ToolDetailRenderer item={tool(
      "todo",
      { action: "create", subject: "修复登录", description: "补上会话过期分支", status: "pending" },
      JSON.stringify({ task: { id: "task-1", subject: "修复登录" } })
    )} />);
    expect(screen.getByText("修复登录")).toBeInTheDocument();
    expect(screen.getByText("补上会话过期分支")).toBeInTheDocument();
    expect(screen.getByText("待处理")).toBeInTheDocument();
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
