import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { ToolDescriptor } from "../types";
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
    name: "preview_start",
    label: "启动预览",
    description: "",
    category: "web",
    dangerous: true,
    parameters: []
  },
  {
    name: "preview_stop",
    label: "停止预览",
    description: "",
    category: "web",
    dangerous: false,
    parameters: []
  },
  {
    name: "preview_list",
    label: "列出预览",
    description: "",
    category: "web",
    dangerous: false,
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
    expect(within(groupDisclosure("文件与搜索")).getByText("1 / 3")).toBeInTheDocument();
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
    expect(filesystemRegion.querySelectorAll(".tool-toggle-row--pick")).toHaveLength(3);
    expect(within(filesystemRegion).queryByText("重复的读取文件")).not.toBeInTheDocument();
    expect(within(filesystemDisclosure).getByText("1 / 3")).toBeInTheDocument();

    // Disabling a group's last tool resets its count without collapsing the group.
    await user.click(within(filesystemRegion).getByRole("button", { name: "代码语义导航已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["powershell", "unknown-tool"]);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(filesystemRegion).not.toHaveAttribute("aria-hidden");
    expect(filesystemRegion).not.toHaveAttribute("inert");
    expect(within(filesystemDisclosure).getByText("0 / 3")).toBeInTheDocument();
    expect(within(filesystemRegion).getByRole("button", { name: "代码语义导航已关闭" })).toBeEnabled();
  });

  it("marks an enabled row pressed and leaves the list in catalog order", async () => {
    const user = userEvent.setup();
    render(<ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />);

    const region = groupRegion(groupDisclosure("文件与搜索"));
    const order = () => Array.from(region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName);
    expect(order()).toEqual(["read", "write", "lsp"]);

    const lsp = within(region).getByRole("button", { name: "代码语义导航已关闭" });
    expect(lsp).toHaveAttribute("aria-pressed", "false");
    await user.click(lsp);

    // Enabling recolours the row in place: no switch, no reordering.
    expect(within(region).getByRole("button", { name: "代码语义导航已启用" }))
      .toHaveAttribute("aria-pressed", "true");
    expect(order()).toEqual(["read", "write", "lsp"]);
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
    expect(within(filesystemDisclosure).getByText("1 / 2")).toBeInTheDocument();
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(Array.from(filesystemRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["read", "write"]);
    // A group whose every tool is spent has nothing left to offer, so it leaves too.
    expect(screen.queryByRole("button", { name: "Shell" })).not.toBeInTheDocument();

    const lockBar = screen.getByRole("button", { name: /已生效的工具/ });
    expect(lockBar).toHaveAttribute("aria-expanded", "false");
    expect(within(lockBar).getByText("3")).toBeInTheDocument();

    await user.click(lockBar);
    const lockRegion = groupRegion(lockBar);
    expect(Array.from(lockRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--locked"))
      .map((row) => row.dataset.toolName)).toEqual(["lsp", "powershell", "agent_spawn"]);
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

  it("lists each preview tool as a row of its own and leaves the lifecycle tools out", () => {
    render(<ControlledGroups initialEnabledTools={["read", "preview_click"]} expansionKey="preset-one" />);

    const disclosure = groupDisclosure("预览");
    expect(within(disclosure).getByText("1 / 2")).toBeInTheDocument();
    const web = groupRegion(disclosure);
    expect(Array.from(web.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["preview_snapshot", "preview_click"]);
    for (const label of ["启动预览", "停止预览", "列出预览"]) {
      expect(screen.queryByText(label)).not.toBeInTheDocument();
    }
  });

  it("switches the lifecycle tools on with the first preview tool and off with the last", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);
    const web = groupRegion(groupDisclosure("预览"));

    await user.click(within(web).getByRole("button", { name: "点击元素已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "preview_click", "preview_start", "preview_stop", "preview_list"
    ]);
    await user.click(within(web).getByRole("button", { name: "页面快照已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "preview_click", "preview_start", "preview_stop", "preview_list", "preview_snapshot"
    ]);

    // Still one preview tool on, so the lifecycle tools stay.
    await user.click(within(web).getByRole("button", { name: "点击元素已启用" }));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "preview_start", "preview_stop", "preview_list", "preview_snapshot"
    ]);
    await user.click(within(web).getByRole("button", { name: "页面快照已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
  });

  it("switches the lifecycle tools with the preview group's bulk pair", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);

    await user.click(groupBulk("预览", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "preview_snapshot", "preview_click", "preview_start", "preview_stop", "preview_list"
    ]);
    expect(within(groupDisclosure("预览")).getByText("2 / 2")).toBeInTheDocument();

    await user.click(groupBulk("预览", "clear"));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
  });

  it("brings a list whose preview tools are on without their lifecycle tools into step on its next edit", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["preview_click"]} expansionKey="preset-one" onChange={onChange} />);

    await user.click(screen.getByRole("button", { name: "代码语义导航已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith([
      "preview_click", "lsp", "preview_start", "preview_stop", "preview_list"
    ]);
  });

  it("keeps a spent lifecycle tool, and never shows it in the lock bar", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["preview_click", "preview_start", "preview_stop", "preview_list"]}
        lockedTools={["preview_start"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    expect(screen.queryByRole("button", { name: /已生效的工具/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "点击元素已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["preview_start"]);
  });

  it("keeps the lifecycle tools while a spent preview tool is still on", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["preview_snapshot", "preview_click", "preview_start", "preview_stop", "preview_list"]}
        lockedTools={["preview_snapshot"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    await user.click(screen.getByRole("button", { name: "点击元素已启用" }));
    expect(onChange).toHaveBeenLastCalledWith([
      "preview_snapshot", "preview_start", "preview_stop", "preview_list"
    ]);
  });

  it("drops manual collapses when expansionKey changes", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    const webDisclosure = groupDisclosure("预览");
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

    await user.click(groupBulk("文件与搜索", "select"));
    expect(onChange).toHaveBeenLastCalledWith(["read", "write", "lsp"]);
    expect(within(groupDisclosure("文件与搜索")).getByText("3 / 3")).toBeInTheDocument();

    // A group's pair reaches the dependants that only ever render under a
    // parent, so the count it reports is the count it can actually move.
    await user.click(groupBulk("代理编排", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "write", "lsp", "agent_spawn", "send_message", "followup_task"
    ]);

    await user.click(groupBulk("文件与搜索", "clear"));
    expect(onChange).toHaveBeenLastCalledWith(["agent_spawn", "send_message", "followup_task"]);
    expect(groupBulk("文件与搜索", "clear")).toBeDisabled();
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

  it("marks a reviewed tool's row", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />
    );

    const marks = (name: string) => Array.from(
      container.querySelectorAll<HTMLElement>(`[data-tool-name="${name}"] em`)
    ).map((mark) => mark.textContent);
    expect(marks("lsp")).toEqual([]);
    expect(marks("powershell")).toEqual(["需审查"]);
  });
});


const fileAndShellTools: ToolDescriptor[] = ([
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

describe("the file and shell tools", () => {
  it("lists every file tool and every shell as a row of its own, in catalog order", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ToolSelectionGroups
        tools={fileAndShellTools}
        enabledTools={["zsh"]}
        expansionKey="files-and-shells"
        onChange={onChange}
      />
    );

    const pickRows = (region: HTMLElement) => Array.from(
      region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick")
    ).map((row) => row.dataset.toolName);
    expect(pickRows(groupRegion(groupDisclosure("文件与搜索"))))
      .toEqual(["ls", "grep", "write", "edit", "find", "read"]);
    expect(pickRows(groupRegion(groupDisclosure("Shell")))).toEqual(["bash", "zsh"]);
    expect(within(groupDisclosure("Shell")).getByText("1 / 2")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Bash已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["zsh", "bash"]);
    expect(screen.getByRole("link", { name: "Bash的说明文档" }).getAttribute("href"))
      .toMatch(/\/tools\/bash\.html$/);
  });
});
