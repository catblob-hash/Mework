import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";
import type { MachineShells, SshMachineConfig, WslDistro } from "../types";

const mocks = vi.hoisted(() => ({
  listWslDistros: vi.fn<() => Promise<WslDistro[]>>()
}));

vi.mock("../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/runtime")>()),
  listWslDistros: mocks.listWslDistros
}));

import { MachineSettingsDialog, type MachineShellsControl } from "./MachineDialogs";

const winbox: SshMachineConfig = {
  id: "winbox",
  name: "winbox",
  host: "dev@winbox",
  port: 0,
  identityFile: "",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z"
};

const windowsProbe: MachineShells = {
  os: "windows",
  shells: [
    { backend: "powershell", path: "C:\\Program Files\\PowerShell\\7\\pwsh.exe" },
    { backend: "bash", path: "C:\\Program Files\\Git\\bin\\bash.exe" }
  ],
  probedAt: "2026-09-23T00:00:00Z"
};

function control(overrides: Partial<MachineShellsControl> = {}): MachineShellsControl {
  return {
    probes: {},
    environments: { sshMachines: [winbox], envVars: {} },
    probe: vi.fn(() => Promise.resolve(windowsProbe)),
    setAgentShell: vi.fn(),
    ...overrides
  };
}

function renderDialog(machine: Parameters<typeof MachineSettingsDialog>[0]["machine"], shells: MachineShellsControl) {
  const handlers = { onSaveSshMachine: vi.fn(), onDeleteSshMachine: vi.fn(), onClose: vi.fn() };
  render(
    <MachineSettingsDialog
      machine={machine}
      sshMachines={[winbox]}
      usage={{ projects: 0, conversations: 0 }}
      shells={shells}
      {...handlers}
    />
  );
  return handlers;
}

beforeEach(() => {
  configureI18n("zh-CN");
  mocks.listWslDistros.mockResolvedValue([{ name: "Ubuntu", version: 2, isDefault: true }]);
});

afterEach(() => vi.clearAllMocks());

describe("a machine's shells in its settings", () => {
  it("lists this machine's shells and probes it again, with no agent shell to choose", async () => {
    const shells = control({
      probes: { local: { os: "macos", shells: [{ backend: "zsh", path: "/bin/zsh" }], probedAt: "" } }
    });
    renderDialog(null, shells);
    const section = screen.getByRole("region", { name: "Shell 后端" });
    expect(within(section).getByText("macOS")).toBeInTheDocument();
    expect(within(section).getByText("/bin/zsh")).toBeInTheDocument();
    // This machine's file tools act on its own filesystem: there is no agent shell.
    expect(within(section).queryByRole("combobox")).toBeNull();
    await userEvent.click(within(section).getByRole("button", { name: "重新探测 shell" }));
    expect(shells.probe).toHaveBeenCalledWith(null);
  });

  it("chooses a WSL distribution's agent shell from what its probe found", async () => {
    const shells = control({
      probes: {
        "wsl:Ubuntu": {
          os: "wsl",
          shells: [{ backend: "bash", path: "/usr/bin/bash" }, { backend: "zsh", path: "/usr/bin/zsh" }],
          probedAt: ""
        }
      }
    });
    renderDialog({ kind: "wsl", distro: "Ubuntu" }, shells);
    const select = screen.getByRole("combobox", { name: "代理 shell" });
    expect(within(select).getAllByRole("option").map((option) => option.textContent)).toEqual(["Bash", "zsh"]);
    expect(select).toHaveValue("bash");
    await userEvent.selectOptions(select, "zsh");
    expect(shells.setAgentShell).toHaveBeenCalledWith({ kind: "wsl", distro: "Ubuntu" }, "zsh");
  });

  it("saves an SSH machine's agent shell with the rest of its settings", async () => {
    const shells = control({ probes: { "ssh:winbox": windowsProbe } });
    const handlers = renderDialog({ kind: "ssh", machineId: "winbox" }, shells);
    const select = screen.getByRole("combobox", { name: "代理 shell" });
    // Nothing chosen yet: Windows' default priority puts PowerShell first.
    expect(select).toHaveValue("powershell");
    await userEvent.selectOptions(select, "bash");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(handlers.onSaveSshMachine).toHaveBeenCalledWith(expect.objectContaining({
      id: "winbox",
      agentShell: "bash"
    }));
  });

  it("says a machine has not been probed and shows why a probe failed", async () => {
    const shells = control({ probe: vi.fn(() => Promise.reject(new Error("ssh: connect to host winbox: timed out"))) });
    renderDialog({ kind: "ssh", machineId: "winbox" }, shells);
    expect(screen.getByText(/还没有探测过这台机器/)).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "代理 shell" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "重新探测 shell" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("timed out"));
  });
});
