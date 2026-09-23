import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { RemoteDirectoryListing } from "../lib/workspacePicker";

const mocks = vi.hoisted(() => ({
  listRemoteDirectory: vi.fn(),
  authorizeRemoteWorkspace: vi.fn()
}));

vi.mock("../lib/workspacePicker", () => ({
  listRemoteDirectory: mocks.listRemoteDirectory,
  authorizeRemoteWorkspace: mocks.authorizeRemoteWorkspace
}));

import { RemoteDirectoryPicker } from "./RemoteDirectoryPicker";

const machine = { kind: "ssh" as const, machineId: "machine-winbox" };

/** A Windows machine as the host reads it: forward slashes, a drive list above the drives. */
const listings: Record<string, RemoteDirectoryListing> = {
  "~": {
    path: "C:/Users/dev",
    parent: "C:/Users",
    entries: [{ name: "中文项目", path: "C:/Users/dev/中文项目" }]
  },
  "C:/Users": {
    path: "C:/Users",
    parent: "C:/",
    entries: [{ name: "dev", path: "C:/Users/dev" }]
  },
  "C:/": {
    path: "C:/",
    parent: "/",
    entries: [{ name: "Users", path: "C:/Users" }]
  },
  "/": {
    path: "/",
    parent: null,
    entries: [
      { name: "C:", path: "C:/" },
      { name: "D:", path: "D:/" }
    ]
  },
  "D:/": { path: "D:/", parent: "/", entries: [] }
};

beforeEach(() => {
  configureI18n("zh-CN");
  mocks.listRemoteDirectory.mockImplementation(async (_machine: unknown, path: string) => {
    const listing = listings[path];
    if (!listing) throw new Error(`unexpected path ${path}`);
    return listing;
  });
});

afterEach(() => {
  vi.clearAllMocks();
  configureI18n("zh-CN");
});

describe("RemoteDirectoryPicker", () => {
  it("walks a Windows machine by the paths the host spelled, up to its drive list", async () => {
    const user = userEvent.setup();
    const onPick = vi.fn();
    render(
      <RemoteDirectoryPicker machine={machine} machineName="winbox" onPick={onPick} onClose={vi.fn()} />
    );
    const picker = await screen.findByRole("dialog", { name: "选择 winbox 上的工作区" });
    expect(await within(picker).findByText("中文项目")).toBeInTheDocument();
    expect(within(picker).getByRole("textbox")).toHaveValue("C:/Users/dev");

    const up = () => within(picker).getByRole("button", { name: "上一级" });
    await user.click(up());
    await waitFor(() => expect(within(picker).getByRole("textbox")).toHaveValue("C:/Users"));
    await user.click(up());
    await waitFor(() => expect(within(picker).getByRole("textbox")).toHaveValue("C:/"));
    await user.click(up());
    await waitFor(() => expect(within(picker).getByRole("textbox")).toHaveValue("/"));
    // The drive list is the top: there is nowhere further up.
    expect(within(picker).queryByRole("button", { name: "上一级" })).toBeNull();

    await user.click(within(picker).getByRole("button", { name: "D:" }));
    await waitFor(() => expect(within(picker).getByRole("textbox")).toHaveValue("D:/"));
    expect(mocks.listRemoteDirectory).toHaveBeenLastCalledWith(machine, "D:/");
    expect(await within(picker).findByText("这个目录里没有子目录")).toBeInTheDocument();
  });

  it("opens an entry at the path the host gave it and confirms the resolved path", async () => {
    const user = userEvent.setup();
    const onPick = vi.fn();
    mocks.authorizeRemoteWorkspace.mockResolvedValue("C:/Users/dev/中文项目");
    listings["C:/Users/dev/中文项目"] = { path: "C:/Users/dev/中文项目", parent: "C:/Users/dev", entries: [] };
    render(
      <RemoteDirectoryPicker machine={machine} machineName="winbox" onPick={onPick} onClose={vi.fn()} />
    );
    const picker = await screen.findByRole("dialog", { name: "选择 winbox 上的工作区" });
    await user.click(await within(picker).findByRole("button", { name: "中文项目" }));
    await waitFor(() =>
      expect(within(picker).getByRole("textbox")).toHaveValue("C:/Users/dev/中文项目")
    );
    await user.click(within(picker).getByRole("button", { name: "选择" }));
    expect(mocks.authorizeRemoteWorkspace).toHaveBeenCalledWith(machine, "C:/Users/dev/中文项目");
    await waitFor(() => expect(onPick).toHaveBeenCalledWith("C:/Users/dev/中文项目"));
  });
});
