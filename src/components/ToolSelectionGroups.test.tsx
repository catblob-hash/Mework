import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { RememberedToolFamilies, ToolDescriptor } from "../types";
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
    name: "powershell",
    label: "PowerShell",
    description: "",
    category: "shell",
    dangerous: true,
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
    // The file tools count as one row, beside the one that keeps its own.
    expect(within(groupDisclosure("文件与搜索")).getByText("1 / 2")).toBeInTheDocument();
    // Exact: the shell row's own label ("Shell 工具设置，…") also contains "Shell".
    expect(within(groupDisclosure("Shell")).getByText("0 / 1")).toBeInTheDocument();
  });

  it("does not collapse a group when its last enabled tool is turned off", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["lsp", "powershell", "unknown-tool"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(filesystemRegion.querySelectorAll(".tool-toggle-row--pick")).toHaveLength(2);
    expect(within(filesystemRegion).queryByText("重复的读取文件")).not.toBeInTheDocument();
    expect(within(filesystemDisclosure).getByText("1 / 2")).toBeInTheDocument();

    // Disabling a group's last tool resets its count without collapsing the group.
    await user.click(within(filesystemRegion).getByRole("button", { name: "代码语义导航已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["powershell", "unknown-tool"]);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(filesystemRegion).not.toHaveAttribute("aria-hidden");
    expect(filesystemRegion).not.toHaveAttribute("inert");
    expect(within(filesystemDisclosure).getByText("0 / 2")).toBeInTheDocument();
    expect(within(filesystemRegion).getByRole("button", { name: "代码语义导航已关闭" })).toBeEnabled();
  });

  it("marks an enabled row pressed and leaves the list in catalog order", async () => {
    const user = userEvent.setup();
    render(<ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />);

    const region = groupRegion(groupDisclosure("文件与搜索"));
    const order = () => Array.from(region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName);
    // The file tools' row stands where the first of them is listed.
    expect(order()).toEqual(["files", "lsp"]);

    const lsp = within(region).getByRole("button", { name: "代码语义导航已关闭" });
    expect(lsp).toHaveAttribute("aria-pressed", "false");
    await user.click(lsp);

    // Enabling recolours the row in place: no switch, no reordering.
    expect(within(region).getByRole("button", { name: "代码语义导航已启用" }))
      .toHaveAttribute("aria-pressed", "true");
    expect(order()).toEqual(["files", "lsp"]);
    expect(within(region).queryAllByRole("switch")).toHaveLength(0);
  });

  it("folds locked tools into one collapsed bar and drops them from the picker", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read", "lsp", "agent_spawn", "powershell"]}
        lockedTools={["lsp", "agent_spawn", "powershell"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    expect(within(filesystemDisclosure).getByText("1 / 1")).toBeInTheDocument();
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(Array.from(filesystemRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["files"]);
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
      .map((row) => row.dataset.toolName)).toEqual(["lsp", "agent_spawn"]);
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

  it("gathers the preview tools behind one row whose gear opens their own settings window", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read", "preview_click"]} expansionKey="preset-one" onChange={onChange} />);

    const web = groupRegion(groupDisclosure("操控"));
    expect(within(web).queryByText("页面快照")).not.toBeInTheDocument();
    expect(within(web).queryByText("点击元素")).not.toBeInTheDocument();

    await user.click(within(web).getByRole("button", { name: "预览工具设置" }));
    const dialog = screen.getByRole("dialog", { name: "预览工具" });
    const categories = within(dialog).getByRole("navigation", { name: "预览工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "开发服务器管理", "查看页面", "操作页面"
    ]);
    // The family is on, so the window edits live tools and says nothing about parking.
    expect(within(dialog).queryByRole("note")).not.toBeInTheDocument();

    await user.click(within(categories).getByRole("button", { name: "查看页面" }));
    await user.click(within(dialog).getByRole("switch", { name: "页面快照已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_click", "preview_snapshot"]);
    expect(within(web).getByRole("button", { name: "预览工具已启用，2 / 2" }).closest(".tool-family-row"))
      .toHaveClass("tool-toggle-row--on");
  });

  it("switches a family off and back on from its row, keeping the tools it had", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read", "preview_click"]} expansionKey="preset-one" onChange={onChange} />);
    const web = groupRegion(groupDisclosure("操控"));

    await user.click(within(web).getByRole("button", { name: "预览工具已启用，1 / 2" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
    // Off, the row counts what switching it on would bring back.
    const off = within(web).getByRole("button", { name: "预览工具已关闭，已选 1 / 2" });
    expect(off).toHaveAttribute("aria-pressed", "false");
    expect(off.closest(".tool-family-row")).not.toHaveClass("tool-toggle-row--on");

    await user.click(off);
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_click"]);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(within(web).getByRole("button", { name: "预览工具已启用，1 / 2" })).toHaveAttribute("aria-pressed", "true");
  });

  it("asks which tools to use when a family with nothing kept is switched on", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);
    const web = groupRegion(groupDisclosure("操控"));

    await user.click(within(web).getByRole("button", { name: "预览工具已关闭，已选 0 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "预览工具" });
    expect(within(dialog).getByRole("note")).toHaveTextContent("关闭窗口时，这里选中的工具随即启用");
    await user.click(within(dialog).getByRole("button", { name: "操作页面" }));
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    // Chosen, not yet switched on: that happens when the window closes.
    expect(onChange).not.toHaveBeenCalledWith(expect.arrayContaining(["preview_click"]));
    expect(within(dialog).getByRole("switch", { name: "点击元素已启用" })).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_click"]);
    expect(within(web).getByRole("button", { name: "预览工具已启用，1 / 2" })).toBeInTheDocument();
  });

  it("leaves a family off when the window its row opened closes with nothing chosen", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);
    const web = groupRegion(groupDisclosure("操控"));

    await user.click(within(web).getByRole("button", { name: "预览工具已关闭，已选 0 / 2" }));
    const dialog = screen.getByRole("dialog", { name: "预览工具" });
    await user.click(within(dialog).getByRole("button", { name: "操作页面" }));
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已关闭" }));
    await user.click(within(dialog).getByRole("switch", { name: "点击元素已启用" }));
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));

    expect(onChange).not.toHaveBeenCalledWith(expect.arrayContaining(["preview_click"]));
    expect(within(web).getByRole("button", { name: "预览工具已关闭，已选 0 / 2" })).toBeInTheDocument();
  });

  it("parks what the gear chooses while the family is off, for its row to switch on", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);
    const web = groupRegion(groupDisclosure("操控"));

    await user.click(within(web).getByRole("button", { name: "预览工具设置" }));
    const dialog = screen.getByRole("dialog", { name: "预览工具" });
    expect(within(dialog).getByRole("note")).toHaveTextContent("这里选中的工具会在打开这组工具时启用");
    await user.click(within(dialog).getByRole("button", { name: "查看页面" }));
    await user.click(within(dialog).getByRole("switch", { name: "页面快照已关闭" }));
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));

    // The gear only configures: nothing is on, but the row now has something to bring back.
    expect(onChange).not.toHaveBeenCalledWith(expect.arrayContaining(["preview_snapshot"]));
    await user.click(within(web).getByRole("button", { name: "预览工具已关闭，已选 1 / 2" }));
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_snapshot"]);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
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
    expect(within(web).getByRole("button", { name: "预览工具已启用，1 / 2" })).toBeInTheDocument();
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
    expect(onChange).toHaveBeenLastCalledWith(["read", "write", "lsp"]);
    expect(within(groupDisclosure("文件与搜索")).getByText("2 / 2")).toBeInTheDocument();

    // A group's pair reaches the dependants that only ever render under a
    // parent, so the count it reports is the count it can actually move.
    await user.click(groupBulk("代理编排", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "write", "lsp", "agent_spawn", "send_message", "followup_task"
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

  it("marks a family's row with the review mark of what it gathers", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />
    );

    const marks = (name: string) => Array.from(
      container.querySelectorAll<HTMLElement>(`[data-tool-name="${name}"] em`)
    ).map((mark) => mark.textContent);
    expect(marks("lsp")).toEqual([]);
    expect(marks("shell")).toEqual(["需审查"]);
  });
});

const previewTools: ToolDescriptor[] = [
  "preview_logs", "preview_console_logs", "preview_snapshot", "preview_click"
].map((name) => ({
  name,
  label: {
    preview_logs: "服务器日志",
    preview_console_logs: "控制台日志",
    preview_snapshot: "页面快照",
    preview_click: "点击元素"
  }[name]!,
  description: "",
  category: "web",
  dangerous: true,
  parameters: []
}));

/** The picker as a conversation owns it: the family rows it keeps are written with the list. */
function KeepingGroups({
  initialEnabledTools,
  lockedTools,
  onWrite,
  onFamilies
}: {
  initialEnabledTools: string[];
  lockedTools?: string[];
  onWrite?: (enabledTools: string[]) => void;
  onFamilies?: (families: RememberedToolFamilies | undefined) => void;
}) {
  const [enabledTools, setEnabledTools] = useState(initialEnabledTools);
  const [families, setFamilies] = useState<RememberedToolFamilies>({});
  return (
    <ToolSelectionGroups
      tools={previewTools}
      enabledTools={enabledTools}
      lockedTools={lockedTools}
      expansionKey="conversation"
      rememberedToolFamilies={families}
      onChange={(next) => {
        onWrite?.(next);
        setEnabledTools(next);
      }}
      onToolSettingsChange={(next) => {
        onWrite?.(next.enabledTools);
        onFamilies?.(next.rememberedToolFamilies);
        setEnabledTools(next.enabledTools);
        if (next.rememberedToolFamilies) setFamilies(next.rememberedToolFamilies);
      }}
    />
  );
}

async function openPreviewPage(user: ReturnType<typeof userEvent.setup>, page: string): Promise<HTMLElement> {
  await user.click(screen.getByRole("button", { name: "预览工具设置" }));
  const dialog = screen.getByRole("dialog", { name: "预览工具" });
  await user.click(within(dialog).getByRole("button", { name: page }));
  return dialog;
}

describe("the preview tool settings window", () => {
  it("keeps a switched-off family's rows with the owner", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    const onFamilies = vi.fn();
    render(
      <KeepingGroups
        initialEnabledTools={["preview_click", "preview_logs"]}
        onWrite={onWrite}
        onFamilies={onFamilies}
      />
    );

    await user.click(screen.getByRole("button", { name: "预览工具已启用，2 / 4" }));
    expect(onWrite).toHaveBeenLastCalledWith([]);
    expect(onFamilies).toHaveBeenLastCalledWith({ preview: ["preview_logs", "preview_click"] });

    // The gear on the family while it is off shows the rows it keeps.
    const dialog = await openPreviewPage(user, "操作页面");
    expect(within(dialog).getByRole("switch", { name: "点击元素已启用" })).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));

    await user.click(screen.getByRole("button", { name: "预览工具已关闭，已选 2 / 4" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_logs", "preview_click"]);
    expect(onFamilies).toHaveBeenLastCalledWith({});
  });

  it("switches off only a family's free tools when some are spent, and brings them back", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    render(
      <KeepingGroups
        initialEnabledTools={["preview_snapshot", "preview_click"]}
        lockedTools={["preview_snapshot"]}
        onWrite={onWrite}
      />
    );

    await user.click(screen.getByRole("button", { name: "预览工具已启用，2 / 4" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_snapshot"]);
    // A spent tool keeps the family on; the row's click now brings the rest back.
    await user.click(screen.getByRole("button", { name: "预览工具已启用，1 / 4" }));
    expect(onWrite).toHaveBeenLastCalledWith(["preview_snapshot", "preview_click"]);
  });

  it("remembers what a bulk clear switches off, and forgets it once a bulk select switches the family on", async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn();
    const onFamilies = vi.fn();
    render(
      <KeepingGroups
        initialEnabledTools={["preview_click", "preview_logs"]}
        onWrite={onWrite}
        onFamilies={onFamilies}
      />
    );
    await user.click(screen.getByRole("button", { name: "全不选操控" }));
    expect(onWrite).toHaveBeenLastCalledWith([]);
    expect(onFamilies).toHaveBeenLastCalledWith({ preview: ["preview_logs", "preview_click"] });

    await user.click(screen.getByRole("button", { name: "全选操控" }));
    expect(onWrite).toHaveBeenLastCalledWith([
      "preview_logs", "preview_console_logs", "preview_snapshot", "preview_click"
    ]);
    expect(onFamilies).toHaveBeenLastCalledWith({});
  });
});

const familyTools: ToolDescriptor[] = ([
  ["ls", "列出文件", "filesystem", false],
  ["grep", "搜索内容", "filesystem", false],
  ["write", "写入文件", "filesystem", true],
  ["edit", "编辑文件", "filesystem", true],
  ["find", "查找文件", "filesystem", false],
  ["read", "读取文件", "filesystem", false],
  ["bash", "Bash", "shell", true],
  ["zsh", "zsh", "shell", true]
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
    expect(pickRows(files)).toEqual(["files"]);

    // Nothing on and nothing kept: the row asks which tools to use.
    await user.click(within(files).getByRole("button", { name: "文件工具已关闭，已选 0 / 6" }));
    const dialog = screen.getByRole("dialog", { name: "文件工具" });
    const categories = within(dialog).getByRole("navigation", { name: "文件工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["文件操作"]);
    expect(within(dialog).getAllByRole("switch").map((control) => control.getAttribute("aria-label")))
      .toEqual(["列出文件已关闭", "查找文件已关闭", "搜索内容已关闭", "读取文件已关闭", "写入文件已关闭", "编辑文件已关闭"]);

    await user.click(within(dialog).getByRole("switch", { name: "读取文件已关闭" }));
    expect(onChange).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
    expect(within(files).getByRole("button", { name: "文件工具已启用，1 / 6" }).closest(".tool-family-row"))
      .toHaveClass("tool-toggle-row--on");
  });

  it("gathers the shells behind one row whose window switches each on its own", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FamilyGroups initialEnabledTools={["zsh"]} onChange={onChange} />);

    const shells = groupRegion(groupDisclosure("Shell"));
    expect(pickRows(shells)).toEqual(["shell"]);

    await user.click(within(shells).getByRole("button", { name: "Shell 工具设置" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });
    const categories = within(dialog).getByRole("navigation", { name: "Shell 工具分类" });
    expect(within(categories).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Shell"]);

    await user.click(within(dialog).getByRole("switch", { name: "Bash已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["zsh", "bash"]);
    expect(within(shells).getByRole("button", { name: "Shell 工具已启用，2 / 2" })).toBeInTheDocument();

    await user.click(within(dialog).getByRole("switch", { name: "Bash已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["zsh"]);
  });

  it("switches the shells on and off in bulk", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<FamilyGroups initialEnabledTools={[]} onChange={onChange} />);

    await user.click(groupBulk("Shell", "select"));
    expect(onChange).toHaveBeenLastCalledWith(["bash", "zsh"]);

    await user.click(groupBulk("Shell", "clear"));
    expect(onChange).toHaveBeenLastCalledWith([]);
  });

  it("leads a family's row to the head of its section and each of its tools to its own page", async () => {
    const user = userEvent.setup();
    render(<FamilyGroups initialEnabledTools={["bash"]} />);

    for (const [family, name] of [["files", "文件工具"], ["shell", "Shell 工具"]] as const) {
      const link = screen.getByRole("link", { name: `${name}的说明文档` });
      // A sibling of the row's button, pointing at the family's section rather than a tool page.
      expect(link.getAttribute("href")).toMatch(new RegExp(`/tools\\.html#${family}$`));
      expect(link).toHaveAttribute("target", "_blank");
    }

    await user.click(screen.getByRole("button", { name: "Shell 工具设置" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });
    const link = within(dialog).getByRole("link", { name: "Bash的说明文档" });
    expect(link.getAttribute("href")).toMatch(/\/tools\/bash\.html$/);
  });

  it("keeps a spent shell switched on in the window", async () => {
    const user = userEvent.setup();
    render(<FamilyGroups initialEnabledTools={["zsh"]} lockedTools={["zsh"]} />);

    expect(screen.queryByRole("button", { name: /已生效的工具/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Shell 工具设置" }));
    const dialog = screen.getByRole("dialog", { name: "Shell 工具" });
    expect(within(dialog).getByRole("switch", { name: "zsh已启用" })).toBeDisabled();
  });
});
