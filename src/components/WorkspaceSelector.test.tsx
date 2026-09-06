import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { createTestDocument as createSeedDocument } from "../test/fixtures";
import { TEMPORARY_WORKSPACE_ID } from "../lib/workspaces";
import { WorkspaceSelector } from "./WorkspaceSelector";

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
        onSelect={onSelect}
        onChooseDirectory={vi.fn()}
        movementDisabled
      />
    );
    expect(screen.getByRole("button", { name: `工作区：${document.workspaces[0].name}` }))
      .toBeDisabled();
  });
});
