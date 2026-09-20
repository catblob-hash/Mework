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
    expect(within(groupDisclosure("文件与搜索")).getByText("1 / 2")).toBeInTheDocument();
    // Exact: a tool row's own label ("PowerShell已关闭") also contains "Shell".
    expect(within(groupDisclosure("Shell")).getByText("0 / 1")).toBeInTheDocument();
  });

  it("does not collapse a group when its last enabled tool is turned off", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read", "write", "powershell", "unknown-tool"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = groupDisclosure("文件与搜索");
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(filesystemRegion.querySelectorAll(".tool-toggle-row--pick")).toHaveLength(2);
    expect(within(filesystemRegion).queryByText("重复的读取文件")).not.toBeInTheDocument();

    await user.click(within(filesystemRegion).getByRole("button", { name: "写入文件已启用" }));
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(within(filesystemDisclosure).getByText("1 / 2")).toBeInTheDocument();

    // Disabling a group's last tool resets its count without collapsing the group.
    await user.click(within(filesystemRegion).getByRole("button", { name: "读取文件已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["powershell", "unknown-tool"]);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(filesystemRegion).not.toHaveAttribute("aria-hidden");
    expect(filesystemRegion).not.toHaveAttribute("inert");
    expect(within(filesystemDisclosure).getByText("0 / 2")).toBeInTheDocument();
    expect(within(filesystemRegion).getByRole("button", { name: "读取文件已关闭" })).toBeEnabled();
  });

  it("marks an enabled row pressed and leaves the list in catalog order", async () => {
    const user = userEvent.setup();
    render(<ControlledGroups initialEnabledTools={[]} expansionKey="preset-one" />);

    const region = groupRegion(groupDisclosure("文件与搜索"));
    const order = () => Array.from(region.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName);
    expect(order()).toEqual(["read", "write"]);

    const write = within(region).getByRole("button", { name: "写入文件已关闭" });
    expect(write).toHaveAttribute("aria-pressed", "false");
    await user.click(write);

    // Enabling recolours the row in place: no switch, no reordering.
    expect(within(region).getByRole("button", { name: "写入文件已启用" }))
      .toHaveAttribute("aria-pressed", "true");
    expect(order()).toEqual(["read", "write"]);
    expect(within(region).queryAllByRole("switch")).toHaveLength(0);
  });

  it("folds locked tools into one collapsed bar and drops them from the picker", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const { container } = render(
      <ControlledGroups
        initialEnabledTools={["read", "write", "powershell"]}
        lockedTools={["read", "powershell"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    // Shell had one tool and it is spent, so the whole group leaves the picker.
    expect(container.querySelector('[data-tool-category="shell"]')).not.toBeInTheDocument();
    const filesystemDisclosure = groupDisclosure("文件与搜索");
    expect(within(filesystemDisclosure).getByText("1 / 1")).toBeInTheDocument();
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(Array.from(filesystemRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--pick"))
      .map((row) => row.dataset.toolName)).toEqual(["write"]);

    const lockBar = screen.getByRole("button", { name: /已生效的工具/ });
    expect(lockBar).toHaveAttribute("aria-expanded", "false");
    expect(within(lockBar).getByText("2")).toBeInTheDocument();

    await user.click(lockBar);
    const lockRegion = groupRegion(lockBar);
    expect(Array.from(lockRegion.querySelectorAll<HTMLElement>(".tool-toggle-row--locked"))
      .map((row) => row.dataset.toolName)).toEqual(["read", "powershell"]);
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

  it("picks every preview tool under one row, and stores their real names", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" onChange={onChange} />);

    const web = groupRegion(groupDisclosure("操控"));
    expect(within(web).queryByText("页面快照")).not.toBeInTheDocument();
    expect(within(web).queryByText("点击元素")).not.toBeInTheDocument();

    await user.click(within(web).getByRole("button", { name: "preview已关闭" }));
    expect(onChange).toHaveBeenLastCalledWith(["read", "preview_snapshot", "preview_click"]);
    expect(within(web).getByRole("button", { name: "preview已启用" })).toBeInTheDocument();

    await user.click(within(web).getByRole("button", { name: "preview已启用" }));
    expect(onChange).toHaveBeenLastCalledWith(["read"]);
  });

  it("shows a partly enabled preview as on, and turns all of it off at once", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read", "preview_click"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const web = groupRegion(groupDisclosure("操控"));
    await user.click(within(web).getByRole("button", { name: "preview已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["read"]);
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
    const sibling = container.querySelector<HTMLElement>('[data-tool-name="read"]')!;
    expect(labelHolder(sibling)).toHaveTextContent("读取文件");

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
    expect(onChange).toHaveBeenLastCalledWith(["read", "write"]);
    expect(within(groupDisclosure("文件与搜索")).getByText("2 / 2")).toBeInTheDocument();

    // A group's pair reaches the dependants that only ever render under a
    // parent, so the count it reports is the count it can actually move.
    await user.click(groupBulk("代理编排", "select"));
    expect(onChange).toHaveBeenLastCalledWith([
      "read", "write", "agent_spawn", "send_message", "followup_task"
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

    const row = container.querySelector<HTMLElement>('[data-tool-name="read"]')!;
    const link = row.parentElement!.querySelector<HTMLAnchorElement>("a.tool-docs-link")!;
    // A sibling of the row's own button rather than a child of it, and a real
    // link: it leaves the application.
    expect(link).toBeInTheDocument();
    expect(link.getAttribute("href")).toContain("/tools/read.html");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAccessibleName("读取文件的说明文档");
  });
});
