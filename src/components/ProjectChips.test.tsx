import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { TEMPORARY_WORKSPACE_ID } from "../lib/workspaces";
import type { AttachedWorkspace, SshMachineConfig, Workspace } from "../types";
import { ProjectSelector, TerminalShellButton, WorkspaceMemberSelector } from "./ProjectChips";

const devbox: SshMachineConfig = {
  id: "machine-devbox",
  name: "devbox",
  host: "dev@devbox",
  port: 0,
  identityFile: "",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z"
};

function projects(): Workspace[] {
  const document = createSeedDocument();
  const base = document.workspaces[0];
  const frontend: Workspace = {
    ...base,
    id: "project-frontend",
    name: "前端项目",
    path: "C:\\frontend",
    conversations: []
  };
  const platform: Workspace = {
    ...base,
    id: "project-platform",
    name: "平台",
    path: "C:\\platform",
    additionalWorkspaces: [{ machine: { kind: "ssh", machineId: devbox.id }, path: "/srv/api" }],
    conversations: []
  };
  const temporary = document.workspaces.find((workspace) => workspace.id === TEMPORARY_WORKSPACE_ID)!;
  return [frontend, platform, temporary];
}

describe("ProjectSelector", () => {
  it("lists the projects, the temporary project and a way to make a new one", async () => {
    const list = projects();
    const onSelect = vi.fn();
    const onCreateProject = vi.fn();
    const user = userEvent.setup();
    render(
      <ProjectSelector
        projects={list}
        activeProject={list[0]}
        sshMachines={[devbox]}
        onSelect={onSelect}
        onCreateProject={onCreateProject}
      />
    );

    await user.click(screen.getByRole("button", { name: "项目：前端项目" }));
    let menu = screen.getByRole("menu", { name: "选择项目" });
    expect(within(menu).getByRole("menuitemradio", { name: /^前端项目/ })).toHaveAttribute("aria-checked", "true");
    // A project with several workspaces says how many, and titles every one of them.
    const platform = within(menu).getByRole("menuitemradio", { name: /^平台/ });
    expect(platform).toHaveTextContent("2");
    expect(platform).toHaveAttribute("title", "C:\\platform\n/srv/api (SSH: devbox)");
    await user.click(platform);
    expect(onSelect).toHaveBeenLastCalledWith("project-platform");

    await user.click(screen.getByRole("button", { name: "项目：前端项目" }));
    menu = screen.getByRole("menu", { name: "选择项目" });
    await user.click(within(menu).getByRole("menuitemradio", { name: "临时项目" }));
    expect(onSelect).toHaveBeenLastCalledWith(TEMPORARY_WORKSPACE_ID);

    await user.click(screen.getByRole("button", { name: "项目：前端项目" }));
    await user.click(within(screen.getByRole("menu", { name: "选择项目" }))
      .getByRole("menuitem", { name: "新建项目…" }));
    expect(onCreateProject).toHaveBeenCalledOnce();
  });

  it("asks for a project when none is chosen yet", () => {
    render(
      <ProjectSelector
        projects={projects()}
        activeProject={null}
        sshMachines={[]}
        onSelect={vi.fn()}
        onCreateProject={vi.fn()}
      />
    );
    expect(screen.getByRole("button", { name: "项目：选择项目" })).toBeInTheDocument();
  });

  it("refuses to move out of or into a project that is being deleted", async () => {
    const list = projects();
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(
      <ProjectSelector
        projects={list}
        activeProject={list[0]}
        sshMachines={[]}
        isProjectDeleting={(id) => id === "project-platform"}
        onSelect={onSelect}
        onCreateProject={vi.fn()}
      />
    );
    await user.click(screen.getByRole("button", { name: "项目：前端项目" }));
    expect(within(screen.getByRole("menu", { name: "选择项目" }))
      .getByRole("menuitemradio", { name: /^平台/ })).toBeDisabled();

    rerender(
      <ProjectSelector
        projects={list}
        activeProject={list[0]}
        sshMachines={[]}
        isProjectDeleting={(id) => id === "project-frontend"}
        onSelect={onSelect}
        onCreateProject={vi.fn()}
      />
    );
    expect(screen.getByRole("button", { name: "项目：前端项目" })).toBeDisabled();
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe("WorkspaceMemberSelector", () => {
  const workspaces: AttachedWorkspace[] = [
    { path: "C:\\platform" },
    { machine: { kind: "ssh", machineId: devbox.id }, path: "/srv/api" },
    { path: "D:\\shared\\tokens" }
  ];

  function renderSelector(props: Partial<Parameters<typeof WorkspaceMemberSelector>[0]> = {}) {
    const handlers = {
      onSelect: vi.fn(),
      onConfigureMachine: vi.fn(),
      onConfigureWorkspace: vi.fn()
    };
    render(
      <WorkspaceMemberSelector
        workspaces={workspaces}
        selected={2}
        sshMachines={[devbox]}
        {...handlers}
        {...props}
      />
    );
    return handlers;
  }

  it("names the selected workspace by its directory and number, and switches between them", async () => {
    const user = userEvent.setup();
    const { onSelect } = renderSelector();

    const trigger = screen.getByRole("button", { name: "工作区：api" });
    expect(trigger).toHaveAttribute("title", "/srv/api (SSH: devbox)");
    expect(trigger).toHaveTextContent("2");
    await user.click(trigger);
    const menu = screen.getByRole("menu", { name: "选择工作区" });
    expect(within(menu).getByRole("menuitemradio", { name: /^api/ })).toHaveAttribute("aria-checked", "true");
    await user.click(within(menu).getByRole("menuitemradio", { name: /^platform/ }));
    expect(onSelect).toHaveBeenCalledWith(1);
  });

  it("groups the workspaces under the machine each is on, keeping their numbers", async () => {
    const user = userEvent.setup();
    renderSelector();

    await user.click(screen.getByRole("button", { name: "工作区：api" }));
    const menu = screen.getByRole("menu", { name: "选择工作区" });
    const sections = Array.from(menu.querySelectorAll(".popover-menu__section")).map((section) => ({
      heading: section.querySelector(".popover-menu__label")?.textContent,
      items: Array.from(section.querySelectorAll(".popover-menu__item")).map((item) => item.textContent)
    }));
    expect(sections).toEqual([
      { heading: "本机", items: ["platform1", "tokens3"] },
      { heading: "SSH: devbox", items: ["api2"] }
    ]);
  });

  it("opens a machine's settings from its heading's gear and a workspace's variables from its own", async () => {
    const user = userEvent.setup();
    const { onSelect, onConfigureMachine, onConfigureWorkspace } = renderSelector();

    await user.click(screen.getByRole("button", { name: "工作区：api" }));
    await user.click(screen.getByRole("button", { name: "SSH: devbox 的设置" }));
    expect(onConfigureMachine).toHaveBeenCalledWith({ kind: "ssh", machineId: devbox.id });
    // The gear closes the menu like any choice, without selecting anything.
    expect(screen.queryByRole("menu", { name: "选择工作区" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "工作区：api" }));
    await user.click(screen.getByRole("button", { name: "本机 的设置" }));
    expect(onConfigureMachine).toHaveBeenLastCalledWith(null);

    await user.click(screen.getByRole("button", { name: "工作区：api" }));
    await user.click(screen.getByRole("button", { name: "tokens 的环境变量" }));
    expect(onConfigureWorkspace).toHaveBeenCalledWith(3);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("leaves the number off a lone workspace, which the model is never told", () => {
    renderSelector({ workspaces: [{ path: "C:\\platform" }], selected: 1, showIndex: false });
    const trigger = screen.getByRole("button", { name: "工作区：platform" });
    expect(trigger.querySelector(".composer-chip__index")).toBeNull();
  });
});

describe("TerminalShellButton", () => {
  it("offers the shells it is given and reports the one picked", async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(<TerminalShellButton workspaceLabel="api" shells={["zsh", "bash", "fish"]} onSelect={onSelect} />);

    await user.click(screen.getByRole("button", { name: "在 api 打开终端" }));
    const menu = screen.getByRole("menu", { name: "用哪个 shell" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["zsh", "bash", "fish"]);
    await user.click(within(menu).getByRole("menuitem", { name: "bash" }));
    expect(onSelect).toHaveBeenCalledWith("bash");
  });

  it("says why it is unavailable", () => {
    render(
      <TerminalShellButton
        workspaceLabel="api"
        shells={["zsh"]}
        disabled
        disabledReason="先发送一条消息再打开终端"
        onSelect={vi.fn()}
      />
    );
    const trigger = screen.getByRole("button", { name: "在 api 打开终端" });
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveAttribute("title", "先发送一条消息再打开终端");
  });
});
