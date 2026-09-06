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
    name: "playwright",
    label: "页面快照",
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
  onChange
}: {
  initialEnabledTools: string[];
  expansionKey: string;
  onChange?: (enabledTools: string[]) => void;
}) {
  const [enabledTools, setEnabledTools] = useState(initialEnabledTools);
  return (
    <ToolSelectionGroups
      tools={tools}
      enabledTools={enabledTools}
      expansionKey={expansionKey}
      onChange={(next) => {
        onChange?.(next);
        setEnabledTools(next);
      }}
    />
  );
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
    expect(within(screen.getByRole("button", { name: /文件与搜索/ })).getByText("1 / 2")).toBeInTheDocument();
    expect(within(screen.getByRole("button", { name: /Shell/ })).getByText("0 / 1")).toBeInTheDocument();
  });

  it("does not collapse a group when its last enabled tool is switched off", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledGroups
        initialEnabledTools={["read", "write", "powershell", "unknown-tool"]}
        expansionKey="preset-one"
        onChange={onChange}
      />
    );

    const filesystemDisclosure = screen.getByRole("button", { name: /文件与搜索/ });
    const filesystemRegion = groupRegion(filesystemDisclosure);
    expect(within(filesystemRegion).getAllByRole("switch")).toHaveLength(2);
    expect(within(filesystemRegion).queryByText("重复的读取文件")).not.toBeInTheDocument();

    await user.click(within(filesystemRegion).getByRole("switch", { name: "写入文件已启用" }));
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(within(filesystemDisclosure).getByText("1 / 2")).toBeInTheDocument();

    // Disabling a group's last tool resets its count without collapsing the group.
    await user.click(within(filesystemRegion).getByRole("switch", { name: "读取文件已启用" }));

    expect(onChange).toHaveBeenLastCalledWith(["powershell", "unknown-tool"]);
    expect(filesystemDisclosure).toHaveAttribute("aria-expanded", "true");
    expect(filesystemRegion).not.toHaveAttribute("aria-hidden");
    expect(filesystemRegion).not.toHaveAttribute("inert");
    expect(within(filesystemDisclosure).getByText("0 / 2")).toBeInTheDocument();
    expect(within(filesystemRegion).getByRole("switch", { name: "读取文件已关闭" })).toBeEnabled();
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

    const disclosure = screen.getByRole("button", { name: /文件与搜索/ });
    expect(disclosure).toHaveAttribute("aria-expanded", "true");

    await user.click(disclosure);

    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    expect(groupRegion(disclosure)).toHaveAttribute("inert");
    expect(onChange).not.toHaveBeenCalled();

    await user.click(disclosure);
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
  });

  it("drops manual collapses when expansionKey changes", async () => {
    const user = userEvent.setup();
    const { rerender } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    const filesystemDisclosure = screen.getByRole("button", { name: /文件与搜索/ });
    const webDisclosure = screen.getByRole("button", { name: /联网/ });
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

    await user.click(screen.getByRole("switch", { name: "子代理已启用" }));
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
    render(<ControlledGroups initialEnabledTools={["read", "task_wait", "task_list"]} expansionKey="preset-one" />);

    expect(screen.queryByText("等待任务")).not.toBeInTheDocument();
    expect(screen.queryByText("任务列表")).not.toBeInTheDocument();
  });

  it("renders the group heading as a single disclosure control without nested buttons", () => {
    const { container } = render(
      <ControlledGroups initialEnabledTools={["read"]} expansionKey="preset-one" />
    );

    expect(container.querySelector("button button")).not.toBeInTheDocument();
    const filesystemGroup = container.querySelector<HTMLElement>('[data-tool-category="filesystem"]')!;
    const heading = filesystemGroup.querySelector<HTMLElement>(".tool-settings-group__heading")!;
    const disclosure = within(heading).getByRole("button", { name: /文件与搜索/ });
    expect(disclosure.parentElement).toBe(heading);
    expect(within(heading).queryAllByRole("switch")).toHaveLength(0);
    expect(heading.children).toHaveLength(1);
  });
});
