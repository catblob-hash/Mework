import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { DecisionParameterModes, RememberedDecisionForms, ToolDescriptor } from "../types";
import { ToolSelectionGroups } from "./ToolSelectionGroups";

const tools: ToolDescriptor[] = [
  {
    name: "read",
    label: "读取文件",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: []
  },
  {
    name: "write",
    label: "写入文件",
    description: "",
    category: "filesystem",
    dangerous: true,
    parameters: []
  },
  {
    name: "read",
    label: "重复的读取文件",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: []
  },
  {
    name: "lsp",
    label: "代码语义导航",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: []
  },
  {
    name: "find_content",
    label: "按描述查找内容",
    description: "",
    category: "filesystem",
    dangerous: false,
    parameters: []
  },
  {
    name: "powershell",
    label: "PowerShell",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: []
  },
  {
    name: "powershell_find_output",
    label: "PowerShell（筛选输出）",
    description: "",
    category: "shell",
    dangerous: true,
    parameters: []
  },
  {
    name: "find_output",
    label: "按描述查找输出",
    description: "",
    category: "shell",
    dangerous: false,
    parameters: []
  },
  {
    name: "preview_snapshot",
    label: "页面快照",
    description: "",
    category: "web",
    dangerous: true,
    parameters: []
  },
  {
    name: "preview_click",
    label: "点击元素",
    description: "",
    category: "web",
    dangerous: true,
    parameters: []
  },
  {
    name: "agent_spawn",
    label: "子代理",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "send_message",
    label: "发送消息",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "followup_task",
    label: "追加任务",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "task_wait",
    label: "等待任务",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
  {
    name: "task_list",
    label: "任务列表",
    description: "",
    category: "orchestration",
    dangerous: false,
    parameters: []
  },
];

function ControlledGroups({
  initialEnabledTools,
  expansionKey,
  lockedTools,
  onChange
}: {
  initialEnabledTools: string[];
  expansionKey: string;
  lockedTools?: string[];
  onChange?: (enabledTools: string[]) => void;
}) {
  const [enabledTools, setEnabledTools] = useState(initialEnabledTools);
  return (
    <ToolSelectionGroups
      tools={tools}
      enabledTools={enabledTools}
      lockedTools={lockedTools}
      expansionKey={expansionKey}
      onChange={(next) => {
        onChange?.(next);
        setEnabledTools(next);
      }}
    />
  );
}

/* The heading now holds three controls — the labelled disclosure, the group's
   select-all / clear-all pair, and an aria-hidden grip on the chevron — and the
   pair's names contain the group's own. So every query for a disclosure names it
   exactly rather than by substring. */
function groupDisclosure(label: string): HTMLElement {
  return screen.getByRole("button", { name: label });
}

function groupBulk(label: string, action: "select" | "clear"): HTMLElement {
  return screen.getByRole("button", {
    name: action === "select" ? `全选${label}` : `全不选${label}`
  });
}

function groupRegion(disclosure: HTMLElement): HTMLElement {
  const regionId = disclosure.getAttribute("aria-controls");
  expect(regionId).toBeTruthy();
  const region = document.getElementById(regionId!);
  expect(region).not.toBeNull();
  return region!;
}

describe("ToolSelectionGroups", () => {
  it("keeps every group expanded and switch-free, whatever is enabled", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={["read", "unknown-tool"]} expansionKey="preset-one" />
    );

    // Categories retain only disclosure buttons; tools are enabled individually.
    expect(screen.queryByRole("switch", { name: /工具组/ })).not.toBeInTheDocument();
    const disclosures = Array.from(
      container.querySelectorAll<HTMLElement>(".tool-settings-group__disclosure")
    );
    expect(disclosures.length).toBeGreaterThan(1);
    // Groups with no enabled tools also start expanded.
    for (const disclosure of disclosures) {
      expect(disclosure).toHaveAttribute("aria-expanded", "true");
      expect(groupRegion(disclosure)).not.toHaveAttribute("inert");
    }
    // The file tools count as one row, beside the two that keep their own.
    expect(within(groupDisclosure("文件与搜索")).getByText("1 / 3")).toBeInTheDocument();
    // Exact: the shell row's own label ("Shell 工具设置，…") also contains "Shell".
    expect(within(groupDisclosure("Shell")).getByText("0 / 2")).toBeInTheDocument();
  });

  it("does not collapse a group when its last enabled tool is turned off", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["lsp", "find_content", "powershell", "unknown-tool"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(filesystemRegion.querySelectorAll(".tool-toggle-row--pick")).toHaveLength(3);
    expect(within(filesystemRegion).queryByText("重复的读取文件")).not.toBeInTheDocument();

    await user.click(within(filesystemRegion).getByRole("button", { name: "代码语义导航已启用" }));
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(within(filesystemDisclosure).getByText("1 / 3")).toBeInTheDocument();

    // Disabling a group's last tool resets its count without collapsing the group.
    await user.click(within(filesystemRegion).getByRole("button", { name: "按描述查找内容已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["powershell", "unknown-tool"]);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(filesystemRegion).not.toHaveAttribute("aria-hidden");
    expect(filesystemRegion).not.toHaveAttribute("inert");
    expect(within(filesystemDisclosure).getByText("0 / 3")).toBeInTheDocument();
    expect(within(filesystemRegion).getByRole("button", { name: "按描述查找内容已关闭" })).toBeEnabled();
  });

  it("marks an enabled row pressed and leaves the list in catalog order", async () => {
    const user = userEvent.setup();
    render(<ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />);

    const region = groupRegion(groupDisclosure("文件与搜索"));
    const order = () => Array.from(region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName);
    // The file tools' row stands where the first of them is listed.
    expect(order()).toEqual(["files", "lsp", "find_content"]);

    const lsp = within(region).getByRole("button", { name: "代码语义导航已关闭" });
    expect(lsp).toHaveAttribute("aria-pressed", "false");
    await user.click(lsp);

    // Enabling recolours the row in place: no switch, no reordering.
    expect(within(region).getByRole("button", { name: "代码语义导航已启用" }))
      .toHaveAttribute("aria-pressed", "true");
    expect(order()).toEqual(["files", "lsp", "find_content"]);
    expect(within(region).queryAllByRole("switch")).toHaveLength(0);
  });

  it("folds locked tools into one collapsed bar and drops them from the picker", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["lsp", "find_content", "find_output", "powershell"]}
        lockedTools={["lsp", "find_output", "powershell"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    expect(within(filesystemDisclosure).getByText("1 / 2")).toBeInTheDocument();
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(Array.from(filesystemRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["files", "find_content"]);
    // A spent shell stays behind the shell row, which never leaves: its window
    // is where the spent backend is seen and the free ones are reached.
    const shellRegion = groupRegion(groupDisclosure("Shell"));
    expect(Array.from(shellRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["shell"]);

    const lockBar = screen.getByRole("button", { name: /已生效的工具/ });
    expect(lockBar).toHaveAttribute("aria-expanded", "false");
    expect(within(lockBar).getByText("2")).toBeInTheDocument();

    await user.click(lockBar);
    const lockRegion = groupRegion(lockBar);
    expect(Array.from(lockRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--locked"))
      .map((row) => row.dataset.toolName)).toEqual(["lsp", "find_output"]);
    // A spent row is a statement, not a control.
    expect(within(lockRegion).queryAllByRole("button")).toHaveLength(0);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("keeps a locked parent's dependants pickable in its place", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const { container } = render(
      <ControlledGroups
        initialEnabledTools={["agent_spawn", "send_message"]}
        lockedTools={["agent_spawn"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    expect(container.querySelector('[data-tool-name="agent_spawn"].tool-toggle-row--pick'))
      .not.toBeInTheDocument();
    expect(Array.from(container.querySelectorAll<HTMLElement>(".tool-toggle-row--nested"))
      .map((row) => row.dataset.toolName)).toEqual(["send_message", "followup_task"]);

    // The dependant is its own name and still free, so it can go back off.
    await user.click(screen.getByRole("button", { name: "发送消息已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["agent_spawn"]);
  });

  it("lets disclosure change expansion without changing enabled tools", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const disclosure = groupDisclosure("文件与搜索");
    expect(disclosure).toHaveAttribute("aria-expanded", "true");

    await user.click(disclosure);

    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    expect(groupRegion(disclosure)).toHaveAttribute("inert");
    expect(onChange).not.toHaveBeenCalled();

    await user.click(disclosure);
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
  });

  it("gathers the preview tools behind one row that opens their own settings window", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);

    const web = groupRegion(groupDisclosure("操控"));
    expect(within(web).queryByText("页面快照")).not.toBeInTheDocument();
    expect(within(web).queryByText("点击元素")).not.toBeInTheDocument();

    await user.click(within(web).getByRole("button", { name: "预览工具设置，已启用 0 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "预览工具" });
    const categories = within(dialog).getByRole("navigation", { name: "预览工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "开发服务器管理", "查看页面", "操作页面"
    ]);

    await user.click(within(categories).getByRole("button", { name: "操作页面" }));
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_click"]);
    expect(within(dialog).getByRole("switch", { name: "点击元素已启用" })).toBeInTheDocument();
    expect(within(web).getByRole("button", { name: "预览工具设置，已启用 1 / 2" }))
      .toHaveClass("tool-toggle-row--on");
  });

  it("keeps the preview row in place when some of its tools are spent", () => {
    render(
      <ControlledGroups
        initialEnabledTools={["read", "preview_click"]}
        lockedTools={["preview_click"]}
        expansionKey="preset-one"
      />
    );

    const web = groupRegion(groupDisclosure("操控"));
    expect(within(web).getByRole("button", { name: "预览工具设置，已启用 1 / 2" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /已生效的工具/ })).not.toBeInTheDocument();
  });

  it("drops manual collapses when expansionKey changes", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    const webDisclosure = groupDisclosure("操控");
    await user.click(filesystemDisclosure);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "false");
    expect(webDisclosure).toHaveAttribute("aria-expanded", "true");

    rerender(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-two" />);

    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(webDisclosure).toHaveAttribute("aria-expanded", "true");
  });

  it("renders subagent children under an enabled parent without a disclosure arrow, and clears them with the parent", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const { container } = render(
      <ControlledGroups
        initialEnabledTools={["read", "agent_spawn", "send_message", "followup_task"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const parent = container.querySelector<HTMLElement>('[data-tool-name="agent_spawn"]')!;
    expect(parent).toHaveTextContent("子代理");
    const nestedRows = container.querySelectorAll<HTMLElement>(".tool-toggle-row--nested");
    expect(nestedRows).toHaveLength(2);
    expect([...nestedRows].map((row) => row.dataset.toolName)).toEqual(["send_message", "followup_task"]);

    // Dependent rows are controlled by the parent toggle, not a parent-row disclosure.
    expect(screen.queryByRole("button", { name: "子代理的从属工具" })).not.toBeInTheDocument();
    expect(parent.querySelector(".tool-toggle-row__disclosure")).toBeNull();
    expect(parent.querySelector(".disclosure-chevron")).toBeNull();
    // The layout targets `.tool-toggle-row > span:first-child`, so the title must
    // be the row's direct first child to align with other tool rows.
    const labelHolder = (row: HTMLElement): HTMLElement => {
      const first = row.firstElementChild as HTMLElement;
      expect(first.tagName).toBe("SPAN");
      expect(first.firstElementChild?.tagName).toBe("STRONG");
      return first;
    };
    expect(labelHolder(parent)).toHaveTextContent("子代理");
    const sibling = container.querySelector<HTMLElement>('[data-tool-name="lsp"]')!;
    expect(labelHolder(sibling)).toHaveTextContent("代码语义导航");

    await user.click(screen.getByRole("button", { name: "子代理已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
    expect(container.querySelector('[data-tool-name="send_message"]')).not.toBeInTheDocument();
    expect(container.querySelector('[data-tool-name="followup_task"]')).not.toBeInTheDocument();
  });

  it("does not render subagent children when their parent is disabled", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={["read", "send_message", "followup_task"]} expansionKey="preset-one" />
    );

    expect(container.querySelector('[data-tool-name="send_message"]')).not.toBeInTheDocument();
    expect(container.querySelector('[data-tool-name="followup_task"]')).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "子代理的从属工具" })).not.toBeInTheDocument();
  });

  it("does not render host-derived task runtime controls", () => {
    render(<ControlledGroups initialEnabledTools={["read", "task_wait", "task_list", "box"]} expansionKey="preset-one" />);

    expect(screen.queryByText("等待任务")).not.toBeInTheDocument();
    expect(screen.queryByText("任务列表")).not.toBeInTheDocument();
    expect(screen.queryByText("后台结果")).not.toBeInTheDocument();
  });

  it("renders the group heading as a labelled disclosure beside its own bulk pair", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    // The bulk pair and the chevron grip are SIBLINGS of the disclosure, never
    // inside it: a button cannot hold another one.
    expect(container.querySelector("button button")).not.toBeInTheDocument();
    const filesystemGroup = container.querySelector<HTMLElement>('[data-tool-category="filesystem"]')!;
    const heading = filesystemGroup.querySelector<HTMLElement>(".tool-settings-group__heading")!;
    const disclosure = groupDisclosure("文件与搜索");
    expect(disclosure.parentElement).toBe(heading);
    expect(within(heading).queryAllByRole("switch")).toHaveLength(0);
    // Disclosure, bulk pair, chevron grip.
    expect(heading.children).toHaveLength(3);
    // The chevron grip is aria-hidden and unfocusable, so assistive technology
    // still finds exactly one control for the disclosure and two for the pair.
    expect(within(heading).getAllByRole("button").map((button) => button.getAttribute("aria-label")))
      .toEqual(["文件与搜索", "全选文件与搜索", "全不选文件与搜索"]);
    const grip = heading.querySelector<HTMLElement>(".tool-settings-group__chevron")!;
    expect(grip).toHaveAttribute("aria-hidden", "true");
    expect(grip).toHaveAttribute("tabindex", "-1");
  });

  it("turns a whole group on and off from its heading, dependants included", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />
    );

    // The file tools' row stands for every file tool, and the pair expands it
    // the same way the row's window would.
    await user.click(groupBulk("文件与搜索", "select"));
    expect(onChange).toHaveBeenLastCalledWith(["read", "write", "lsp", "find_content"]);
    expect(within(groupDisclosure("文件与搜索")).getByText("3 / 3")).toBeInTheDocument();

    // A group's pair reaches the dependants that only ever render under a
    // parent, so the count it reports is the count it can actually move.
    await user.click(groupBulk("代理编排", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "write", "lsp", "find_content", "agent_spawn", "send_message", "followup_task"
    ]);

    await user.click(groupBulk("文件与搜索", "clear"));
    expect(onChange).toHaveBeenLastCalledWith(["agent_spawn", "send_message", "followup_task"]);
    expect(groupBulk("文件与搜索", "clear")).toBeDisabled();
    // The preview row stands for several names, and the group pair expands it
    // the same way the row itself does.
    await user.click(groupBulk("操控", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "agent_spawn", "send_message", "followup_task", "preview_snapshot", "preview_click"
    ]);
  });

  it("opens a collapsed group when it is selected into, and leaves it closed when cleared", async () => {
    const user = userEvent.setup();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />);

    const disclosure = groupDisclosure("文件与搜索");
    await user.click(disclosure);
    expect(disclosure).toHaveAttribute("aria-expanded", "false");

    // Selecting into a collapsed group would otherwise report a new count with
    // nothing on screen to account for it.
    await user.click(groupBulk("文件与搜索", "select"));
    expect(disclosure).toHaveAttribute("aria-expanded", "true");

    await user.click(disclosure);
    await user.click(groupBulk("文件与搜索", "clear"));
    expect(disclosure).toHaveAttribute("aria-expanded", "false");
  });

  it("leaves a locked group's row untouched when a bulk pair sweeps its category", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read", "write"]}
        lockedTools={["read"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    // `read` is spent: it is not in the picker and the pair cannot take it back.
    await user.click(groupBulk("文件与搜索", "clear"));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
  });

  it("gives every tool row its own way in to that tool's documentation", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    const row = container.querySelector<HTMLElement>('[data-tool-name="lsp"]')!;
    const link = row.parentElement!.querySelector<HTMLAnchorElement>("a.tool-docs-link")!;
    // A sibling of the row's own button rather than a child of it, and a real
    // link: it leaves the application.
    expect(link).toBeInTheDocument();
    expect(link.getAttribute("href")).toContain("/tools/lsp.html");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAccessibleName("代码语义导航的说明文档");
  });

  it("marks the tools that run through the decision model beside the review mark", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />
    );

    const marks = (name: string) => Array.from(
      container.querySelectorAll<HTMLElement>(`[data-tool-name="${name}"] em`)
    ).map((mark) => [mark.textContent, mark.classList.contains("tool-toggle-row__decision")]);
    expect(marks("find_content")).toEqual([["决策模型", true]]);
    expect(marks("find_output")).toEqual([["决策模型", true]]);
    expect(marks("lsp")).toEqual([]);
    // A family's row carries the review mark of what it gathers, and no decision mark.
    expect(marks("shell")).toEqual([["需审查", false]]);
  });
});

const previewTools: ToolDescriptor[] = [
  "preview_logs", "preview_find_logs", "preview_console_logs", "preview_snapshot", "preview_click"
].map((name) => ({
  name,
  label: {
    preview_logs: "服务器日志",
    preview_find_logs: "按描述查找日志",
    preview_console_logs: "控制台日志",
    preview_snapshot: "页面快照",
    preview_click: "点击元素"
  }[name]!,
  description: "",
  category: "web",
  dangerous: true,
  parameters: []
}));

function ModedGroups({
  catalog = previewTools,
  initialEnabledTools,
  initialModes = {},
  initialScoring = [],
  initialRemembered = {},
  lockedTools,
  lockedModes,
  onWrite,
  onScoring,
  onRemember
}: {
  catalog?: ToolDescriptor[];
  initialEnabledTools: string[];
  initialModes?: DecisionParameterModes;
  initialScoring?: string[];
  initialRemembered?: RememberedDecisionForms;
  lockedTools?: string[];
  lockedModes?: DecisionParameterModes;
  onWrite?: (enabledTools: string[], modes?: DecisionParameterModes) => void;
  onScoring?: (scoring: string[]) => void;
  onRemember?: (remembered: RememberedDecisionForms) => void;
}) {
  const [enabledTools, setEnabledTools] = useState(initialEnabledTools);
  const [modes, setModes] = useState(initialModes);
  const [scoring, setScoring] = useState(initialScoring);
  const [remembered, setRemembered] = useState(initialRemembered);
  return (
    <ToolSelectionGroups
      tools={catalog}
      enabledTools={enabledTools}
      lockedTools={lockedTools}
      expansionKey="conversation"
      decisionParameterModes={modes}
      lockedDecisionParameterModes={lockedModes}
      decisionMissScoring={scoring}
      rememberedDecisionForms={remembered}
      onChange={(next) => {
        onWrite?.(next);
        setEnabledTools(next);
      }}
      onToolSettingsChange={(next) => {
        onWrite?.(next.enabledTools, next.decisionParameterModes);
        onScoring?.(next.decisionMissScoring);
        onRemember?.(next.rememberedDecisionForms);
        setEnabledTools(next.enabledTools);
        setModes(next.decisionParameterModes);
        setScoring(next.decisionMissScoring);
        setRemembered(next.rememberedDecisionForms);
      }}
    />
  );
}

async function openPreviewPage(user: ReturnType<typeof userEvent.setup>, page: string): Promise<HTMLElement> {
  await user.click(screen.getByRole("button", { name: /^预览工具设置/ }));
  const dialog = screen.getByRole("dialog", { name: "预览工具" });
  await user.click(within(dialog).getByRole("button", { name: page }));
  return dialog;
}

function formsRegion(dialog: HTMLElement, tool: string): HTMLElement {
  const entry = dialog.querySelector(`[data-tool-name="${tool}"]`);
  expect(entry).not.toBeNull();
  return entry!.querySelector(".collapse-region") as HTMLElement;
}

describe("the preview tool settings window", () => {
  it("slides a tool's two decision forms out only while the tool is on, and keeps them exclusive", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    render(<ModedGroups initialEnabledTools={[]} onWrite={onWrite} />);
    const dialog = await openPreviewPage(user, "操作页面");

    expect(formsRegion(dialog, "preview_click")).toHaveClass("collapse-region--closed");
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    expect(formsRegion(dialog, "preview_click")).not.toHaveClass("collapse-region--closed");

    await user.click(within(dialog).getByRole("switch", { name: "点击元素：加上决策模型参数" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_click"], { preview_click: "augment" });

    // The second form replaces the first rather than joining it.
    await user.click(within(dialog).getByRole("switch", { name: "点击元素：只用决策模型" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_click"], { preview_click: "replace" });
    expect(within(dialog).getByRole("switch", { name: "点击元素：加上决策模型参数" }))
      .toHaveAttribute("aria-checked", "false");

    // Off takes the live form away and keeps it for the next time the tool comes on.
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已启用" }));
    expect(onWrite).toHaveBeenLastCalledWith([], {});
    expect(formsRegion(dialog, "preview_click")).toHaveClass("collapse-region--closed");

    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_click"], { preview_click: "replace" });
    expect(within(dialog).getByRole("switch", { name: "点击元素：只用决策模型" }))
      .toHaveAttribute("aria-checked", "true");
  });

  it("reads preview_logs' forms off which of the two log tools are on", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    render(<ModedGroups initialEnabledTools={[]} onWrite={onWrite} />);
    const dialog = await openPreviewPage(user, "开发服务器管理");

    // The variant is a choice under its row, never a row of its own.
    expect(within(dialog).queryByRole("switch", { name: "按描述查找日志已关闭" })).not.toBeInTheDocument();
    await user.click(within(dialog).getByRole("switch", { name: "服务器日志已关闭" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_logs"], {});

    await user.click(within(dialog).getByRole("switch", { name: "服务器日志：加上决策模型参数" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_logs", "preview_find_logs"], {});

    await user.click(within(dialog).getByRole("switch", { name: "服务器日志：只用决策模型" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_find_logs"], {});
    // The row stays on while its variant is.
    expect(within(dialog).getByRole("switch", { name: "服务器日志已启用" })).toBeInTheDocument();

    await user.click(within(dialog).getByRole("switch", { name: "服务器日志已启用" }));
    expect(onWrite).toHaveBeenLastCalledWith([], {});

    // Both log tools are off, so the choice lives in the remembered forms, and
    // switching the row back on reads it from there.
    await user.click(within(dialog).getByRole("switch", { name: "服务器日志已关闭" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_find_logs"], {});
    expect(within(dialog).getByRole("switch", { name: "服务器日志：只用决策模型" }))
      .toHaveAttribute("aria-checked", "true");
  });

  it("never offers a form that would narrow what the transcript already holds", async () => {
    const user = userEvent.setup();
    render(
      <ModedGroups
        initialEnabledTools={["preview_click", "preview_snapshot"]}
        initialModes={{ preview_snapshot: "replace" }}
        lockedTools={["preview_click", "preview_snapshot"]}
        lockedModes={{ preview_snapshot: "replace" }}
      />
    );
    let dialog = await openPreviewPage(user, "操作页面");
    // Selector calls went out: the tool stays on, and dropping the selector is refused.
    expect(within(dialog).getByRole("switch", { name: "点击元素已启用" })).toBeDisabled();
    expect(within(dialog).getByRole("switch", { name: "点击元素：只用决策模型" })).toBeDisabled();
    expect(within(dialog).getByRole("switch", { name: "点击元素：加上决策模型参数" })).toBeEnabled();

    dialog = screen.getByRole("dialog", { name: "预览工具" });
    await user.click(within(dialog).getByRole("button", { name: "查看页面" }));
    // Query-only calls went out: back to the direct form alone is refused, widening is not.
    expect(within(dialog).getByRole("switch", { name: "页面快照：只用决策模型" })).toBeDisabled();
    expect(within(dialog).getByRole("switch", { name: "页面快照：加上决策模型参数" })).toBeEnabled();
  });

  it("offers only the tool-backed forms where the owner keeps no modes", async () => {
    const user = userEvent.setup();
    render(<ControlledPreviewRole />);
    const dialog = await openPreviewPage(user, "查看页面");
    expect(within(dialog).queryByRole("switch", { name: "页面快照：加上决策模型参数", hidden: true }))
      .not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "开发服务器管理" }));
    expect(within(dialog).getByRole("switch", { name: "服务器日志：加上决策模型参数" })).toBeInTheDocument();
  });

  it("switches the base tools on in bulk and leaves the variants to the window", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    render(<ModedGroups initialEnabledTools={[]} onWrite={onWrite} />);
    await user.click(screen.getByRole("button", { name: "全选操控" }));
    expect(onWrite).toHaveBeenLastCalledWith([
      "preview_logs", "preview_console_logs", "preview_snapshot", "preview_click"
    ], {});
  });

  it("scores an element tool's misses only while it has a decision form", async () => {
    const user = userEvent.setup();
    const onScoring = vi.fn();
    render(<ModedGroups initialEnabledTools={["preview_click"]} onScoring={onScoring} />);
    const dialog = await openPreviewPage(user, "操作页面");
    const missSwitch = () => within(dialog).getByRole("switch", { name: "点击元素：未命中时逐个打分" });

    // On the direct form there is nothing to miss.
    expect(missSwitch()).toBeDisabled();
    await user.click(within(dialog).getByRole("switch", { name: "点击元素：加上决策模型参数" }));
    expect(missSwitch()).toBeEnabled();
    await user.click(missSwitch());
    expect(onScoring).toHaveBeenLastCalledWith(["preview_click"]);
    expect(missSwitch()).toHaveAttribute("aria-checked", "true");

    // Changing forms keeps it; going back to the direct form takes it away.
    await user.click(within(dialog).getByRole("switch", { name: "点击元素：只用决策模型" }));
    expect(onScoring).toHaveBeenLastCalledWith(["preview_click"]);
    await user.click(within(dialog).getByRole("switch", { name: "点击元素：只用决策模型" }));
    expect(onScoring).toHaveBeenLastCalledWith([]);
    expect(missSwitch()).toHaveAttribute("aria-checked", "false");

    // The scoring tools only: the snapshot already scores and has no miss.
    await user.click(within(dialog).getByRole("button", { name: "查看页面" }));
    expect(within(dialog).queryByRole("switch", { name: /页面快照：未命中时逐个打分/, hidden: true }))
      .not.toBeInTheDocument();
  });

  it("keeps the miss scoring of a tool switched off for when it comes back", async () => {
    const user = userEvent.setup();
    const onScoring = vi.fn();
    const onRemember = vi.fn();
    render(
      <ModedGroups
        initialEnabledTools={["preview_click"]}
        initialModes={{ preview_click: "augment" }}
        initialScoring={["preview_click"]}
        onScoring={onScoring}
        onRemember={onRemember}
      />
    );
    const dialog = await openPreviewPage(user, "操作页面");
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已启用" }));
    expect(onScoring).toHaveBeenLastCalledWith([]);
    expect(onRemember).toHaveBeenLastCalledWith({ preview_click: { form: "augment", missScoring: true } });

    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    expect(onScoring).toHaveBeenLastCalledWith(["preview_click"]);
    expect(onRemember).toHaveBeenLastCalledWith({});
    expect(within(dialog).getByRole("switch", { name: "点击元素：未命中时逐个打分" }))
      .toHaveAttribute("aria-checked", "true");
  });

  it("forgets a tool switched off on its direct form", async () => {
    const user = userEvent.setup();
    const onRemember = vi.fn();
    render(
      <ModedGroups
        initialEnabledTools={["preview_click"]}
        initialRemembered={{ preview_click: { form: "replace" } }}
        onRemember={onRemember}
      />
    );
    const dialog = await openPreviewPage(user, "操作页面");
    // Nothing to keep: the direct form is where a row starts anyway, and an
    // older choice would otherwise come back in its place.
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已启用" }));
    expect(onRemember).toHaveBeenLastCalledWith({});
  });

  it("remembers what a bulk clear switches off and restores it on a bulk select", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    const onScoring = vi.fn();
    render(
      <ModedGroups
        initialEnabledTools={["preview_click", "preview_find_logs"]}
        initialModes={{ preview_click: "augment" }}
        initialScoring={["preview_click"]}
        onWrite={onWrite}
        onScoring={onScoring}
      />
    );
    await user.click(screen.getByRole("button", { name: "全不选操控" }));
    expect(onWrite).toHaveBeenLastCalledWith([], {});
    expect(onScoring).toHaveBeenLastCalledWith([]);

    await user.click(screen.getByRole("button", { name: "全选操控" }));
    expect(onWrite).toHaveBeenLastCalledWith(
      ["preview_find_logs", "preview_console_logs", "preview_snapshot", "preview_click"],
      { preview_click: "augment" }
    );
    expect(onScoring).toHaveBeenLastCalledWith(["preview_click"]);
  });

  it("marks every sub-option as a decision-model option", async () => {
    const user = userEvent.setup();
    render(<ModedGroups initialEnabledTools={["preview_click"]} />);
    const dialog = await openPreviewPage(user, "操作页面");
    const forms = formsRegion(dialog, "preview_click");
    const titles = Array.from(forms.querySelectorAll<HTMLElement>(".tool-family-settings__row-title"));
    expect(titles.map((title) => title.querySelector("strong")?.textContent)).toEqual([
      "加上决策模型参数", "只用决策模型", "未命中时逐个打分"
    ]);
    for (const title of titles) {
      expect(title.querySelector("em.tool-family-settings__decision")).toHaveTextContent("决策模型");
    }
  });
});

function ControlledPreviewRole() {
  const [enabledTools, setEnabledTools] = useState<string[]>(["preview_snapshot", "preview_logs"]);
  return (
    <ToolSelectionGroups
      tools={previewTools}
      enabledTools={enabledTools}
      expansionKey="role"
      onChange={setEnabledTools}
    />
  );
}

const familyTools: ToolDescriptor[] = ([
  ["ls", "列出文件", "filesystem", false],
  ["grep", "搜索内容", "filesystem", false],
  ["write", "写入文件", "filesystem", true],
  ["edit", "编辑文件", "filesystem", true],
  ["find", "查找文件", "filesystem", false],
  ["read", "读取文件", "filesystem", false],
  ["find_content", "按描述查找内容", "filesystem", false],
  ["bash", "Bash", "shell", true],
  ["bash_find_output", "Bash（筛选输出）", "shell", true],
  ["zsh", "zsh", "shell", true],
  ["zsh_find_output", "zsh（筛选输出）", "shell", true],
  ["find_output", "按描述查找输出", "shell", false]
] as const).map(([name, label, category, dangerous]) => ({
  name,
  label,
  description: "",
  category,
  dangerous,
  parameters: []
}));

function FamilyGroups({
  initialEnabledTools,
  lockedTools,
  onChange
}: {
  initialEnabledTools: string[];
  lockedTools?: string[];
  onChange?: (enabledTools: string[]) => void;
}) {
  const [enabledTools, setEnabledTools] = useState(initialEnabledTools);
  return (
    <ToolSelectionGroups
      tools={familyTools}
      enabledTools={enabledTools}
      lockedTools={lockedTools}
      expansionKey="families"
      onChange={(next) => {
        onChange?.(next);
        setEnabledTools(next);
      }}
    />
  );
}

function pickRows(region: HTMLElement): (string | undefined)[] {
  return Array.from(region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
    .map((row) => row.dataset.toolName);
}

describe("the file and shell tool families", () => {
  it("gathers the six file tools behind one row whose window has a single page", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FamilyGroups initialEnabledTools={[]} onChange={onChange} />);

    const files = groupRegion(groupDisclosure("文件与搜索"));
    // The decision-model tool keeps its own row beside the family's.
    expect(pickRows(files)).toEqual(["files", "find_content"]);

    await user.click(within(files).getByRole("button", { name: "文件工具设置，已启用 0 / 6" }));
    const dialog = screen.getByRole("dialog", { name: "文件工具" });
    const categories = within(dialog).getByRole("navigation", { name: "文件工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["文件操作"]);
    expect(within(dialog).getAllByRole("switch").map((control) => control.getAttribute("aria-label")))
      .toEqual(["列出文件已关闭", "查找文件已关闭", "搜索内容已关闭", "读取文件已关闭", "写入文件已关闭", "编辑文件已关闭"]);

    await user.click(within(dialog).getByRole("switch", { name: "读取文件已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
    expect(within(files).getByRole("button", { name: "文件工具设置，已启用 1 / 6" }))
      .toHaveClass("tool-toggle-row--on");
  });

  it("gathers the shells behind one row and slides each one's scored form out under it", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FamilyGroups initialEnabledTools={[]} onChange={onChange} />);

    const shells = groupRegion(groupDisclosure("Shell"));
    expect(pickRows(shells)).toEqual(["shell", "find_output"]);

    await user.click(within(shells).getByRole("button", { name: "Shell 工具设置，已启用 0 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });
    const categories = within(dialog).getByRole("navigation", { name: "Shell 工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Shell"]);
    // The scored tool is a choice under its shell, never a row of its own.
    expect(within(dialog).queryByRole("switch", { name: "Bash（筛选输出）已关闭" })).not.toBeInTheDocument();

    expect(formsRegion(dialog, "bash")).toHaveClass("collapse-region--closed");
    await user.click(within(dialog).getByRole("switch", { name: "Bash已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["bash"]);
    expect(formsRegion(dialog, "bash")).not.toHaveClass("collapse-region--closed");

    await user.click(within(dialog).getByRole("switch", { name: "Bash：加上决策模型参数" }));
    expect(onChange).toHaveBeenLastCalledWith(["bash", "bash_find_output"]);

    await user.click(within(dialog).getByRole("switch", { name: "Bash：只用决策模型" }));
    expect(onChange).toHaveBeenLastCalledWith(["bash_find_output"]);
    // The row stays on while its scored form is, and counts once.
    expect(within(dialog).getByRole("switch", { name: "Bash已启用" })).toBeInTheDocument();
    expect(within(shells).getByRole("button", { name: "Shell 工具设置，已启用 1 / 2" })).toBeInTheDocument();

    await user.click(within(dialog).getByRole("switch", { name: "Bash已启用" }));
    expect(onChange).toHaveBeenLastCalledWith([]);
  });

  it("switches the shells on in bulk and leaves their scored forms to the window", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FamilyGroups initialEnabledTools={[]} onChange={onChange} />);

    await user.click(groupBulk("Shell", "select"));
    expect(onChange).toHaveBeenLastCalledWith(["bash", "zsh", "find_output"]);

    await user.click(groupBulk("Shell", "clear"));
    expect(onChange).toHaveBeenLastCalledWith([]);
  });

  it("brings a shell back with the scored form it was switched off with", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    const onRemember = vi.fn();
    render(
      <ModedGroups
        catalog={familyTools}
        initialEnabledTools={["bash", "bash_find_output"]}
        onWrite={onWrite}
        onRemember={onRemember}
      />
    );
    await user.click(screen.getByRole("button", { name: "Shell 工具设置，已启用 1 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });

    await user.click(within(dialog).getByRole("switch", { name: "Bash已启用" }));
    expect(onWrite).toHaveBeenLastCalledWith([], {});
    expect(onRemember).toHaveBeenLastCalledWith({ bash: { form: "augment" } });

    await user.click(within(dialog).getByRole("switch", { name: "Bash已关闭" }));
    expect(onWrite).toHaveBeenLastCalledWith(["bash", "bash_find_output"], {});
    expect(onRemember).toHaveBeenLastCalledWith({});
  });

  it("keeps a spent shell switched on in the window", async () => {
    const user = userEvent.setup();
    render(<FamilyGroups initialEnabledTools={["zsh", "zsh_find_output"]} lockedTools={["zsh"]} />);

    expect(screen.queryByRole("button", { name: /已生效的工具/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Shell 工具设置，已启用 1 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });
    expect(within(dialog).getByRole("switch", { name: "zsh已启用" })).toBeDisabled();
    // Plain zsh calls went out, so dropping plain zsh is refused; keeping both is not.
    expect(within(dialog).getByRole("switch", { name: "zsh：只用决策模型" })).toBeDisabled();
    expect(within(dialog).getByRole("switch", { name: "zsh：加上决策模型参数" })).toBeEnabled();
  });
});
