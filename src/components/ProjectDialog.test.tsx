import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { AttachedWorkspace, SshMachineConfig, WslDistro } from "../types";

const mocks = vi.hoisted(() => ({
  listWslDistros: vi.fn<() => Promise<WslDistro[]>>(),
  listRemoteDirectory: vi.fn(),
  authorizeRemoteWorkspace: vi.fn()
}));

vi.mock("../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/runtime")>()),
  listWslDistros: mocks.listWslDistros
}));
vi.mock("../lib/workspacePicker", () => ({
  listRemoteDirectory: mocks.listRemoteDirectory,
  authorizeRemoteWorkspace: mocks.authorizeRemoteWorkspace
}));

import { ProjectDialog, projectWorkspaceKey, splitPathForDisplay } from "./ProjectDialog";
import type { ProjectDialogProps } from "./ProjectDialog";

const devbox: SshMachineConfig = {
  id: "machine-devbox",
  name: "devbox",
  host: "dev@devbox",
  port: 0,
  identityFile: "",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z"
};

function renderDialog(props: Partial<ProjectDialogProps> = {}) {
  const handlers = {
    onPickLocalDirectory: vi.fn<() => Promise<string | null>>().mockResolvedValue(null),
    onSaveSshMachine: vi.fn(),
    onSubmit: vi.fn(),
    onClose: vi.fn()
  };
  const view = render(
    <ProjectDialog
      mode="create"
      sshMachines={[devbox]}
      envVars={{}}
      showWsl
      nativePicker
      {...handlers}
      {...props}
    />
  );
  return { ...view, ...handlers };
}

const submitButton = () => screen.getByRole("button", { name: "创建项目" });
const rows = () => Array.from(document.querySelectorAll<HTMLElement>(".project-dialog__row"));
const machineChip = (index: number) => screen.getByRole("button", {
  name: new RegExp(`^工作区 ${index} 的机器：`)
});

beforeEach(() => {
  configureI18n("zh-CN");
  mocks.listWslDistros.mockReset().mockResolvedValue([
    { name: "Ubuntu", version: 2, isDefault: true },
    { name: "Debian", version: 2, isDefault: false }
  ]);
  mocks.listRemoteDirectory.mockReset();
  mocks.authorizeRemoteWorkspace.mockReset();
});

afterEach(() => configureI18n("zh-CN"));

describe("splitPathForDisplay", () => {
  it("keeps the last segment whole and every separator as written", () => {
    expect(splitPathForDisplay("C:\\Users\\me\\app")).toEqual({ head: "C:\\Users\\me", tail: "\\app" });
    expect(splitPathForDisplay("/srv/www/app")).toEqual({ head: "/srv/www", tail: "/app" });
    expect(splitPathForDisplay("~/code")).toEqual({ head: "~", tail: "/code" });
    expect(splitPathForDisplay("/srv/app/")).toEqual({ head: "/srv", tail: "/app/" });
    expect(splitPathForDisplay("/srv")).toEqual({ head: "", tail: "/srv" });
    expect(splitPathForDisplay("/")).toEqual({ head: "", tail: "/" });
    expect(splitPathForDisplay("C:\\")).toEqual({ head: "", tail: "C:\\" });
    expect(splitPathForDisplay("\\\\server\\share\\dir")).toEqual({ head: "\\\\server\\share", tail: "\\dir" });
  });

  it("folds case only for Windows-looking paths when comparing rows", () => {
    expect(projectWorkspaceKey(null, "C:\\Work\\App")).toBe(projectWorkspaceKey(null, "c:\\work\\app"));
    expect(projectWorkspaceKey(null, "/work/App")).not.toBe(projectWorkspaceKey(null, "/work/app"));
    expect(projectWorkspaceKey({ kind: "wsl", distro: "Ubuntu" }, "/srv"))
      .not.toBe(projectWorkspaceKey(null, "/srv"));
  });
});

describe("ProjectDialog", () => {
  it("starts with one empty local row and adds and removes rows", async () => {
    const user = userEvent.setup();
    renderDialog();

    expect(screen.getByRole("dialog", { name: "新建项目" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("留空时使用第一个工作区的文件夹名称")).toBeInTheDocument();
    expect(screen.getByText("移除项目只会从 Mework 取消注册，不会删除任何本地文件。")).toBeInTheDocument();
    expect(rows()).toHaveLength(1);
    expect(machineChip(1)).toHaveTextContent("本机");
    expect(machineChip(1)).toHaveClass("composer-chip");
    expect(screen.getByRole("button", { name: "为工作区 1 选择目录" })).toHaveTextContent("选择目录…");
    // The primary row has nothing to remove.
    expect(screen.queryByRole("button", { name: "移除工作区 1" })).toBeNull();
    expect(submitButton()).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "添加工作区" }));
    await user.click(screen.getByRole("button", { name: "添加工作区" }));
    expect(rows()).toHaveLength(3);
    expect(machineChip(3)).toHaveTextContent("本机");
    expect(screen.getByRole("button", { name: "移除工作区 2" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "移除工作区 2" }));
    expect(rows()).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "移除工作区 3" })).toBeNull();
  });

  it("stops adding rows at the per-project limit", async () => {
    const user = userEvent.setup();
    renderDialog();
    const add = screen.getByRole("button", { name: "添加工作区" });
    for (let count = 1; count < 16; count += 1) await user.click(add);

    expect(rows()).toHaveLength(16);
    expect(add).toBeDisabled();
    expect(add).toHaveAttribute("title", "一个项目最多 16 个工作区");
  });

  it("offers Local, WSL and SSH, with the SSH list ending in an add item", async () => {
    const user = userEvent.setup();
    renderDialog({
      initialWorkspaces: [{ path: "/Users/me/app" }]
    });

    await user.click(machineChip(1));
    const menu = screen.getByRole("menu", { name: "选择机器" });
    expect(menu).toHaveClass("project-dialog__menu");
    expect(mocks.listWslDistros).toHaveBeenCalledTimes(1);
    expect(within(menu).getByRole("menuitemradio", { name: "本机" })).toHaveAttribute("aria-checked", "true");

    await user.click(within(menu).getByRole("menuitem", { name: "WSL" }));
    const wsl = await screen.findByRole("menu", { name: "WSL" });
    expect(await within(wsl).findByRole("menuitemradio", { name: /Ubuntu/ })).toBeInTheDocument();
    expect(within(wsl).getByRole("menuitemradio", { name: /Debian/ })).toBeInTheDocument();

    await user.click(within(menu).getByRole("menuitem", { name: "SSH" }));
    const ssh = screen.getByRole("menu", { name: "SSH" });
    const sshItems = Array.from(ssh.querySelectorAll(".popover-menu__item"));
    expect(sshItems.map((item) => item.textContent)).toEqual(["devbox", "添加 SSH 机器…"]);

    // Choosing another machine clears the path chosen on the old one.
    await user.click(within(ssh).getByRole("menuitemradio", { name: "devbox" }));
    expect(screen.queryByRole("menu", { name: "选择机器" })).toBeNull();
    expect(machineChip(1)).toHaveTextContent("devbox");
    expect(screen.getByRole("button", { name: "为工作区 1 选择目录" })).toBeInTheDocument();
  });

  it("opens the machine menu above its chip when there is room there", async () => {
    // jsdom lays nothing out; give the chip a place low in the window and the panel a height.
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const box = this.classList.contains("popover-menu__panel")
        ? { left: 0, top: 0, width: 220, height: 110 }
        : this.getAttribute("aria-haspopup") === "menu"
          ? { left: 100, top: 520, width: 80, height: 25 }
          : { left: 0, top: 0, width: 0, height: 0 };
      return { ...box, x: box.left, y: box.top, right: box.left + box.width, bottom: box.top + box.height, toJSON: () => box } as DOMRect;
    });
    try {
      const user = userEvent.setup();
      renderDialog();
      await user.click(machineChip(1));
      const menu = screen.getByRole("menu", { name: "选择机器" });
      expect(menu).toHaveClass("popover-menu__panel--flipped");
      expect(menu.style.top).toBe(`${520 - 6 - 110}px`);
    } finally {
      vi.restoreAllMocks();
    }
  });

  it("hides WSL when the host has none to offer and says so when a distro list is empty", async () => {
    const user = userEvent.setup();
    const { unmount } = renderDialog({ showWsl: false });
    await user.click(machineChip(1));
    let menu = screen.getByRole("menu", { name: "选择机器" });
    expect(within(menu).queryByRole("menuitem", { name: "WSL" })).toBeNull();
    expect(within(menu).getByRole("menuitem", { name: "SSH" })).toBeInTheDocument();
    expect(mocks.listWslDistros).not.toHaveBeenCalled();
    unmount();

    mocks.listWslDistros.mockResolvedValue([]);
    renderDialog();
    await user.click(machineChip(1));
    menu = screen.getByRole("menu", { name: "选择机器" });
    await user.click(within(menu).getByRole("menuitem", { name: "WSL" }));
    const wsl = screen.getByRole("menu", { name: "WSL" });
    expect(await within(wsl).findByRole("menuitem", { name: "没有 WSL 发行版" })).toBeDisabled();
  });

  it("registers a machine from the SSH submenu's add item and selects it for that row", async () => {
    const user = userEvent.setup();
    const { onSaveSshMachine } = renderDialog();
    await user.click(screen.getByRole("button", { name: "添加工作区" }));

    await user.click(machineChip(2));
    await user.click(screen.getByRole("menuitem", { name: "SSH" }));
    await user.click(screen.getByRole("menuitem", { name: "添加 SSH 机器…" }));

    const dialog = screen.getByRole("dialog", { name: "添加 SSH 机器" });
    await user.type(within(dialog).getByLabelText("名称"), "buildbox");
    await user.type(within(dialog).getByLabelText("主机"), "ci@build");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(onSaveSshMachine).toHaveBeenCalledTimes(1);
    const [machine, vars] = onSaveSshMachine.mock.calls[0];
    expect(machine).toMatchObject({ name: "buildbox", host: "ci@build", port: 0 });
    expect(vars).toEqual({});
    expect(screen.queryByRole("dialog", { name: "添加 SSH 机器" })).toBeNull();
    // The caller's catalog has not caught up yet; the row still names the new machine.
    expect(machineChip(2)).toHaveTextContent("buildbox");
    expect(machineChip(1)).toHaveTextContent("本机");
  });

  it("shows the full path with a middle ellipsis: a shrinking head and a whole last segment", () => {
    const path = "C:\\Users\\me\\very\\deep\\projects\\app";
    renderDialog({ initialWorkspaces: [{ path }] });

    const button = screen.getByRole("button", { name: `工作区 1：${path}` });
    expect(button).toHaveClass("composer-chip", "project-dialog__path");
    expect(button).toHaveAttribute("title", path);
    const head = button.querySelector(".project-dialog__path-head");
    const tail = button.querySelector(".project-dialog__path-tail");
    expect(head?.textContent).toBe("C:\\Users\\me\\very\\deep\\projects");
    expect(tail?.textContent).toBe("\\app");
    expect(button.querySelector(".project-dialog__path-text")?.textContent).toBe(path);
  });

  it("picks a local directory through the host picker and submits the rows", async () => {
    const user = userEvent.setup();
    const { onPickLocalDirectory, onSubmit } = renderDialog();
    onPickLocalDirectory.mockResolvedValueOnce(null);

    await user.click(screen.getByRole("button", { name: "为工作区 1 选择目录" }));
    expect(onPickLocalDirectory).toHaveBeenCalledTimes(1);
    // Cancelling leaves the row empty.
    expect(screen.getByRole("button", { name: "为工作区 1 选择目录" })).toBeInTheDocument();
    expect(submitButton()).toBeDisabled();

    onPickLocalDirectory.mockResolvedValueOnce("/Users/me/app");
    await user.click(screen.getByRole("button", { name: "为工作区 1 选择目录" }));
    expect(await screen.findByRole("button", { name: "工作区 1：/Users/me/app" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "添加工作区" }));
    expect(submitButton()).toBeDisabled();
    onPickLocalDirectory.mockResolvedValueOnce("/Users/me/lib");
    await user.click(screen.getByRole("button", { name: "为工作区 2 选择目录" }));
    await screen.findByRole("button", { name: "工作区 2：/Users/me/lib" });

    await user.type(screen.getByPlaceholderText("留空时使用第一个工作区的文件夹名称"), "  My app  ");
    expect(submitButton()).toBeEnabled();
    await user.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith("My app", [
      { path: "/Users/me/app" },
      { path: "/Users/me/lib" }
    ] satisfies AttachedWorkspace[]);
  });

  it("shows a failing picker's error on its row", async () => {
    const user = userEvent.setup();
    const { onPickLocalDirectory } = renderDialog();
    onPickLocalDirectory.mockRejectedValueOnce(new Error("portal unavailable"));

    await user.click(screen.getByRole("button", { name: "为工作区 1 选择目录" }));
    expect(await within(rows()[0]).findByRole("alert"))
      .toHaveTextContent("无法打开目录选择器：portal unavailable");
  });

  it("browses a remote row's machine and records the path the host authorized", async () => {
    const user = userEvent.setup();
    mocks.listRemoteDirectory.mockResolvedValue({ path: "/home/dev", directories: ["app"], hasParent: true });
    mocks.authorizeRemoteWorkspace.mockResolvedValue("/home/dev");
    const { onSubmit } = renderDialog({
      initialWorkspaces: [{ machine: { kind: "ssh", machineId: devbox.id }, path: "" }]
    });

    await user.click(screen.getByRole("button", { name: "为工作区 1 选择目录" }));
    const picker = await screen.findByRole("dialog", { name: "选择 devbox 上的工作区" });
    expect(mocks.listRemoteDirectory).toHaveBeenCalledWith({ kind: "ssh", machineId: devbox.id }, "~");
    await waitFor(() => expect(within(picker).getByRole("button", { name: "选择" })).toBeEnabled());
    await user.click(within(picker).getByRole("button", { name: "选择" }));

    expect(await screen.findByRole("button", { name: "工作区 1：/home/dev" })).toBeInTheDocument();
    await user.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith("", [{ machine: { kind: "ssh", machineId: devbox.id }, path: "/home/dev" }]);
  });

  it("flags a row that repeats an earlier one on the same machine and blocks submit", async () => {
    const user = userEvent.setup();
    renderDialog({
      initialWorkspaces: [
        { path: "C:\\Work\\App" },
        { path: "c:\\work\\app" },
        { machine: { kind: "wsl", distro: "Ubuntu" }, path: "C:\\Work\\App" }
      ]
    });

    expect(within(rows()[1]).getByRole("alert")).toHaveTextContent("与工作区 1 是同一台机器上的同一个目录");
    // The same path on another machine is another directory.
    expect(within(rows()[2]).queryByRole("alert")).toBeNull();
    expect(submitButton()).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "移除工作区 2" }));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(submitButton()).toBeEnabled();
  });

  it("locks the primary workspace when editing, while members can still be removed and added", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderDialog({
      mode: "edit",
      initialName: "Shop",
      initialWorkspaces: [
        { path: "/Users/me/shop" },
        { machine: { kind: "ssh", machineId: devbox.id }, path: "/srv/shop" },
        { path: "/Users/me/shared" }
      ]
    });

    expect(screen.getByRole("dialog", { name: "编辑项目" })).toBeInTheDocument();
    expect(screen.getByDisplayValue("Shop")).toBeInTheDocument();
    expect(machineChip(1)).toBeDisabled();
    expect(screen.getByRole("button", { name: "工作区 1：/Users/me/shop" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "移除工作区 1" })).toBeNull();
    expect(machineChip(2)).toBeEnabled();
    expect(machineChip(2)).toHaveTextContent("devbox");

    await user.click(screen.getByRole("button", { name: "移除工作区 3" }));
    await user.click(screen.getByRole("button", { name: "添加工作区" }));
    const save = screen.getByRole("button", { name: "保存" });
    expect(save).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "移除工作区 3" }));
    expect(save).toBeEnabled();

    await user.click(save);
    expect(onSubmit).toHaveBeenCalledWith("Shop", [
      { path: "/Users/me/shop" },
      { machine: { kind: "ssh", machineId: devbox.id }, path: "/srv/shop" }
    ]);
  });

  it("takes a typed absolute path for a local row in the browser preview", async () => {
    const user = userEvent.setup();
    const { onSubmit, onPickLocalDirectory } = renderDialog({ nativePicker: false });

    const input = screen.getByRole("textbox", { name: "工作区 1 的绝对路径" });
    await user.type(input, "  /tmp/project ");
    await user.click(submitButton());
    expect(onPickLocalDirectory).not.toHaveBeenCalled();
    expect(onSubmit).toHaveBeenCalledWith("", [{ path: "/tmp/project" }]);
  });

  it("lets Escape close an open machine menu without closing the dialog", async () => {
    const user = userEvent.setup();
    const { onClose } = renderDialog();
    await user.click(machineChip(1));
    expect(screen.getByRole("menu", { name: "选择机器" })).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu", { name: "选择机器" })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
