import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { TEMPORARY_WORKSPACE_ID } from "../lib/workspaces";
import type { SshMachineConfig } from "../types";
import { WorkspaceSelector } from "./WorkspaceSelector";

const noMachines = { wslDistros: null, sshMachines: [] };

const devbox: SshMachineConfig = {
  id: "machine-devbox",
  name: "devbox",
  host: "dev@devbox",
  port: 0,
  identityFile: "",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z"
};

describe("WorkspaceSelector", () => {
  it("offers directory and temporary-workspace destinations", async () => {
    const document = createSeedDocument();
    const firstDirectory = {
      ...document.workspaces[0],
      id: "workspace-frontend",
      name: "前端项目",
      path: "C:\\frontend",
      conversations: []
    };
    const secondDirectory = {
      ...document.workspaces[0],
      id: "workspace-backend",
      name: "后端项目",
      path: "C:\\backend",
      conversations: []
    };
    document.workspaces.splice(document.workspaces.length - 1, 0, firstDirectory, secondDirectory);
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={document.workspaces[0]}
        machines={noMachines}
        onSelect={onSelect}
        onChooseDirectory={vi.fn()}
      />
    );

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    let menu = screen.getByRole("menu", { name: "选择工作区" });
    expect(within(menu).getByRole("menuitemradio", { name: "前端项目" })).toBeInTheDocument();
    expect(within(menu).getByRole("menuitemradio", { name: "后端项目" })).toBeInTheDocument();
    expect(within(menu).getByRole("menuitemradio", { name: "临时工作区" })).toBeInTheDocument();
    await user.click(within(menu).getByRole("menuitemradio", { name: "前端项目" }));
    expect(onSelect).toHaveBeenLastCalledWith(firstDirectory.id);

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    menu = screen.getByRole("menu", { name: "选择工作区" });
    await user.click(within(menu).getByRole("menuitemradio", { name: "临时工作区" }));
    expect(onSelect).toHaveBeenLastCalledWith(TEMPORARY_WORKSPACE_ID);
  });

  it("asks which machine to choose a directory on", async () => {
    const document = createSeedDocument();
    const onChooseDirectory = vi.fn();
    const onOpen = vi.fn();
    const user = userEvent.setup();
    render(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={document.workspaces[0]}
        machines={{ wslDistros: [{ name: "Ubuntu", version: 2, isDefault: true }], sshMachines: [devbox] }}
        onOpen={onOpen}
        onSelect={vi.fn()}
        onChooseDirectory={onChooseDirectory}
      />
    );

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    // Opening is when the distributions are enumerated: one can be installed while the app runs.
    expect(onOpen).toHaveBeenCalledTimes(1);
    const menu = screen.getByRole("menu", { name: "选择工作区" });
    const choose = within(menu).getByRole("menuitem", { name: "选择工作区" });
    expect(choose).toHaveAttribute("aria-haspopup", "menu");
    // Expanding lists the machines rather than opening a dialog straight away.
    await user.click(choose);
    expect(onChooseDirectory).not.toHaveBeenCalled();
    const machines = within(menu).getByRole("menu", { name: "选择工作区" });
    expect(within(machines).getByRole("menuitem", { name: "本机" })).toBeInTheDocument();
    expect(within(machines).getByRole("menuitem", { name: "Ubuntu" })).toBeInTheDocument();
    await user.click(within(machines).getByRole("menuitem", { name: "devbox" }));
    expect(onChooseDirectory).toHaveBeenLastCalledWith({ kind: "ssh", machineId: devbox.id });

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitem", { name: "选择工作区" }));
    await user.click(screen.getByRole("menuitem", { name: "Ubuntu" }));
    expect(onChooseDirectory).toHaveBeenLastCalledWith({ kind: "wsl", distro: "Ubuntu" });

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitem", { name: "选择工作区" }));
    await user.click(screen.getByRole("menuitem", { name: "本机" }));
    expect(onChooseDirectory).toHaveBeenLastCalledWith(null);
  });

  it("chooses on this machine directly when no host picker can browse another", async () => {
    const document = createSeedDocument();
    const onChooseDirectory = vi.fn();
    const user = userEvent.setup();
    render(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={document.workspaces[0]}
        machines={{ wslDistros: null, sshMachines: [devbox] }}
        remoteMachines={false}
        onSelect={vi.fn()}
        onChooseDirectory={onChooseDirectory}
      />
    );
    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    await user.click(within(screen.getByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitem", { name: "选择工作区" }));
    expect(onChooseDirectory).toHaveBeenCalledWith(null);
    expect(screen.queryByRole("menuitem", { name: "devbox" })).toBeNull();
  });

  it("names the machine of a workspace that is not on this one", async () => {
    const document = createSeedDocument();
    const remote = {
      ...document.workspaces[0],
      id: "workspace-remote",
      name: "services",
      path: "/home/dev/services",
      machine: { kind: "ssh" as const, machineId: devbox.id },
      conversations: []
    };
    document.workspaces.splice(document.workspaces.length - 1, 0, remote);
    const user = userEvent.setup();
    render(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={remote}
        machines={{ wslDistros: null, sshMachines: [devbox] }}
        onSelect={vi.fn()}
        onChooseDirectory={vi.fn()}
      />
    );
    const trigger = screen.getByRole("button", { name: "工作区：services" });
    expect(trigger).toHaveAttribute("title", "/home/dev/services (SSH: devbox)");
    await user.click(trigger);
    expect(within(screen.getByRole("menu", { name: "选择工作区" })).getByRole("menuitemradio", { name: "services" }))
      .toHaveAttribute("title", "/home/dev/services (SSH: devbox)");
  });

  it("disables source and target workspaces that are being deleted", async () => {
    const document = createSeedDocument();
    const target = {
      ...document.workspaces[0],
      id: "workspace-deleting",
      name: "删除中工作区",
      path: "C:\\deleting",
      conversations: []
    };
    document.workspaces.splice(document.workspaces.length - 1, 0, target);
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={document.workspaces[0]}
        machines={noMachines}
        onSelect={onSelect}
        onChooseDirectory={vi.fn()}
        isWorkspaceDeleting={(workspaceId) => workspaceId === target.id}
      />
    );

    await user.click(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }));
    const targetButton = within(screen.getByRole("menu", { name: "选择工作区" }))
      .getByRole("menuitemradio", { name: target.name });
    expect(targetButton).toBeDisabled();
    await user.click(targetButton);
    expect(onSelect).not.toHaveBeenCalled();

    rerender(
      <WorkspaceSelector
        workspaces={document.workspaces}
        activeWorkspace={document.workspaces[0]}
        machines={noMachines}
        onSelect={onSelect}
        onChooseDirectory={vi.fn()}
        movementDisabled
      />
    );
    expect(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }))
      .toBeDisabled();
  });
});
