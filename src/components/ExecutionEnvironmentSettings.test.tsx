import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import { createTestDocument } from "../test/fixtures";
import type {
  AppDocument,
  ExecutionEnvironmentAssets,
  GlobalSettings as GlobalSettingsType,
  SshMachineConfig,
  WslDistro
} from "../types";

const mocks = vi.hoisted(() => ({
  listWslDistros: vi.fn<() => Promise<WslDistro[]>>()
}));

vi.mock("../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/runtime")>()),
  listWslDistros: mocks.listWslDistros
}));

import { ExecutionEnvironmentSettings, sshMachineUsage } from "./ExecutionEnvironmentSettings";
import { GlobalSettings } from "./GlobalSettings";

const devbox: SshMachineConfig = {
  id: "machine-devbox",
  name: "devbox",
  host: "dev@devbox",
  port: 2222,
  identityFile: "",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z"
};

const spare: SshMachineConfig = { ...devbox, id: "machine-spare", name: "spare", host: "spare", port: 0 };

/** A document where devbox is the first workspace of one project, a member of another, and attached to one conversation. */
function documentUsingDevbox(): AppDocument {
  const document = createTestDocument();
  const [first] = document.workspaces;
  first.machine = { kind: "ssh", machineId: devbox.id };
  document.workspaces.push({
    ...first,
    id: "ws_second",
    name: "Second",
    path: "C:\\second",
    machine: null,
    additionalWorkspaces: [{ machine: { kind: "ssh", machineId: devbox.id }, path: "/srv/second" }],
    conversations: []
  });
  document.workspaces.push({
    ...first,
    id: "ws_third",
    name: "Third",
    path: "C:\\third",
    machine: null,
    additionalWorkspaces: [{ path: "D:\\shared" }],
    conversations: first.conversations.map((conversation, index) => ({
      ...conversation,
      id: `${conversation.id}_copy`,
      attachedWorkspaces: index === 0 ? [{ machine: { kind: "ssh", machineId: devbox.id }, path: "~/x" }] : []
    }))
  });
  return document;
}

function renderPage(
  initial: ExecutionEnvironmentAssets,
  document: AppDocument | null = null
) {
  let current = initial;
  function Harness() {
    const [environments, setEnvironments] = useState(initial);
    current = environments;
    return (
      <ExecutionEnvironmentSettings
        environments={environments}
        document={document}
        onChange={(update) => setEnvironments((previous) => update(previous))}
      />
    );
  }
  render(<Harness />);
  return { environments: () => current };
}

beforeEach(() => {
  configureI18n("zh-CN");
  mocks.listWslDistros.mockReset().mockResolvedValue([]);
});

afterEach(() => configureI18n("zh-CN"));

describe("sshMachineUsage", () => {
  it("counts projects by any of their workspaces and conversations by their attached ones", () => {
    expect(sshMachineUsage(documentUsingDevbox(), devbox.id)).toEqual({ projects: 2, conversations: 1 });
    expect(sshMachineUsage(documentUsingDevbox(), spare.id)).toEqual({ projects: 0, conversations: 0 });
    expect(sshMachineUsage(null, devbox.id)).toEqual({ projects: 0, conversations: 0 });
  });
});

describe("ExecutionEnvironmentSettings", () => {
  it("edits this machine's variables and hides WSL when there are no distributions", async () => {
    const user = userEvent.setup();
    const page = renderPage({ sshMachines: [], envVars: { local: { A: "1", B: "2" } } });

    expect(screen.getByRole("heading", { name: "执行环境" })).toBeInTheDocument();
    await waitFor(() => expect(mocks.listWslDistros).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("heading", { name: "WSL 发行版" })).toBeNull();
    expect(screen.getByText("2 个环境变量")).toBeInTheDocument();
    expect(screen.getByText(/还没有登记 SSH 机器/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "编辑 本机 的环境变量" }));
    const dialog = screen.getByRole("dialog", { name: "本机 的环境变量" });
    const textarea = within(dialog).getByRole("textbox", { name: "环境变量" });
    expect(textarea).toHaveValue("A=1\nB=2");
    await user.clear(textarea);
    await user.type(textarea, "PROXY=http://127.0.0.1:7890");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(page.environments().envVars).toEqual({ local: { PROXY: "http://127.0.0.1:7890" } });
    expect(screen.getByText("1 个环境变量")).toBeInTheDocument();
  });

  it("lists WSL distributions with their own variable tables", async () => {
    const user = userEvent.setup();
    mocks.listWslDistros.mockResolvedValue([
      { name: "Ubuntu", version: 2, isDefault: true },
      { name: "Debian", version: 1, isDefault: false }
    ]);
    const page = renderPage({ sshMachines: [], envVars: { "wsl:Debian": { X: "y" } } });

    expect(await screen.findByRole("heading", { name: "WSL 发行版" })).toBeInTheDocument();
    expect(screen.getByText("WSL 2 · 默认")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "编辑 Ubuntu 的环境变量" }));
    const dialog = screen.getByRole("dialog", { name: "Ubuntu 的环境变量" });
    await user.type(within(dialog).getByRole("textbox", { name: "环境变量" }), "GOPATH=/go");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(page.environments().envVars).toEqual({ "wsl:Debian": { X: "y" }, "wsl:Ubuntu": { GOPATH: "/go" } });
  });

  it("adds and edits SSH machines", async () => {
    const user = userEvent.setup();
    const page = renderPage({ sshMachines: [devbox], envVars: { "ssh:machine-devbox": { K: "v" } } });

    const list = screen.getByRole("heading", { name: "SSH 机器" }).closest(".execution-env__section") as HTMLElement;
    expect(within(list).getByText("devbox")).toBeInTheDocument();
    expect(within(list).getByText("dev@devbox:2222")).toBeInTheDocument();
    expect(within(list).getByText("1 个环境变量")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "添加 SSH 机器…" }));
    let dialog = screen.getByRole("dialog", { name: "添加 SSH 机器" });
    // Nothing to delete while creating.
    expect(within(dialog).queryByRole("button", { name: "删除" })).toBeNull();
    await user.type(within(dialog).getByLabelText("名称"), "buildbox");
    await user.type(within(dialog).getByLabelText("主机"), "ci@build");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));
    expect(page.environments().sshMachines.map((machine) => machine.name)).toEqual(["devbox", "buildbox"]);

    await user.click(screen.getByRole("button", { name: "编辑 SSH 机器 devbox" }));
    dialog = screen.getByRole("dialog", { name: "配置 SSH 机器" });
    const host = within(dialog).getByLabelText("主机");
    await user.clear(host);
    await user.type(host, "dev@devbox.lan");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(page.environments().sshMachines[0]).toMatchObject({ id: devbox.id, host: "dev@devbox.lan" });
    expect(page.environments().envVars["ssh:machine-devbox"]).toEqual({ K: "v" });
  });

  it("confirms a delete with what still uses the machine, then drops the machine and its variables", async () => {
    const user = userEvent.setup();
    const page = renderPage(
      { sshMachines: [devbox, spare], envVars: { "ssh:machine-devbox": { K: "v" }, local: { A: "1" } } },
      documentUsingDevbox()
    );

    await user.click(screen.getByRole("button", { name: "编辑 SSH 机器 devbox" }));
    const dialog = screen.getByRole("dialog", { name: "配置 SSH 机器" });
    const remove = within(dialog).getByRole("button", { name: "删除" });
    expect(remove).toHaveAttribute("title", "2 个项目、1 个对话仍在使用这台机器");
    await user.click(remove);

    let confirm = screen.getByRole("dialog", { name: "删除 SSH 机器“devbox”？" });
    expect(confirm).toHaveTextContent("2 个项目、1 个对话仍在使用这台机器。");
    await user.click(within(confirm).getByRole("button", { name: "取消" }));
    // Cancelling returns to the machine dialog with nothing deleted.
    expect(screen.getByRole("dialog", { name: "配置 SSH 机器" })).toBeInTheDocument();
    expect(page.environments().sshMachines).toHaveLength(2);

    await user.click(within(screen.getByRole("dialog", { name: "配置 SSH 机器" })).getByRole("button", { name: "删除" }));
    confirm = screen.getByRole("dialog", { name: "删除 SSH 机器“devbox”？" });
    await user.click(within(confirm).getByRole("button", { name: "删除" }));

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(page.environments()).toEqual({ sshMachines: [spare], envVars: { local: { A: "1" } } });
  });

  it("says when nothing uses a machine being deleted", async () => {
    const user = userEvent.setup();
    renderPage({ sshMachines: [spare], envVars: {} }, documentUsingDevbox());

    await user.click(screen.getByRole("button", { name: "编辑 SSH 机器 spare" }));
    const remove = within(screen.getByRole("dialog", { name: "配置 SSH 机器" })).getByRole("button", { name: "删除" });
    expect(remove).not.toHaveAttribute("title");
    await user.click(remove);
    expect(screen.getByRole("dialog", { name: "删除 SSH 机器“spare”？" }))
      .toHaveTextContent("没有项目或对话在使用这台机器。");
  });

  it("is a global settings page that writes through the settings change handler", async () => {
    const user = userEvent.setup();
    const document = createTestDocument();
    let latest: GlobalSettingsType = document.globalSettings;
    function Harness() {
      const [settings, setSettings] = useState(document.globalSettings);
      latest = settings;
      return (
        <GlobalSettings
          initialView="execution_environments"
          settings={settings}
          document={document}
          onChange={(change) => setSettings((current) => (typeof change === "function" ? change(current) : change))}
        />
      );
    }
    render(<Harness />);

    const navigation = screen.getByRole("navigation", { name: "全局设置分类" });
    expect(within(navigation).getByRole("button", { name: "执行环境" })).toHaveClass("settings-nav__item--active");
    await user.click(screen.getByRole("button", { name: "编辑 本机 的环境变量" }));
    const dialog = screen.getByRole("dialog", { name: "本机 的环境变量" });
    await user.type(within(dialog).getByRole("textbox", { name: "环境变量" }), "TOKEN_FILE=~/.token");
    await user.click(within(dialog).getByRole("button", { name: "保存" }));

    expect(latest.executionEnvironments.envVars.local).toEqual({ TOKEN_FILE: "~/.token" });
    expect(latest.apiProviders).toBe(document.globalSettings.apiProviders);
  });
});
