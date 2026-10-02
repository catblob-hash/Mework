import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SandboxSupport } from "../../types";
import { SandboxSettings } from ".";

const runtimeMocks = vi.hoisted(() => ({
  machineSandboxSupport: vi.fn(),
  setupLocalSandbox: vi.fn()
}));

vi.mock("../../lib/runtime", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/runtime")>()),
  ...runtimeMocks
}));

const NEEDS_SETUP: SandboxSupport = {
  backend: "srt-win",
  available: false,
  detail: "The Windows sandbox needs a one-time setup",
  setup: true
};

describe("SandboxSettings", () => {
  beforeEach(() => {
    runtimeMocks.machineSandboxSupport.mockReset();
    runtimeMocks.setupLocalSandbox.mockReset();
  });

  it("offers the one-time Windows setup and shows the sandbox available after it", async () => {
    runtimeMocks.machineSandboxSupport.mockResolvedValue(NEEDS_SETUP);
    runtimeMocks.setupLocalSandbox.mockResolvedValue({ backend: "srt-win", available: true, detail: "", setup: false });
    render(<SandboxSettings machine={null} machineName="这台电脑" settings={undefined} onChange={vi.fn()} />);

    await userEvent.click(await screen.findByRole("button", { name: /Set up|设置/ }));

    expect(runtimeMocks.machineSandboxSupport).toHaveBeenCalledWith(null);
    expect(runtimeMocks.setupLocalSandbox).toHaveBeenCalledTimes(1);
    expect(await screen.findByText(/Windows sandbox$|Windows 沙箱$/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Set up|设置/ })).not.toBeInTheDocument();
  });

  it("keeps the setup offered and says why when it fails", async () => {
    runtimeMocks.machineSandboxSupport.mockResolvedValue(NEEDS_SETUP);
    runtimeMocks.setupLocalSandbox.mockRejectedValue("The setup was cancelled at the administrator prompt");
    render(<SandboxSettings machine={null} machineName="这台电脑" settings={undefined} onChange={vi.fn()} />);

    await userEvent.click(await screen.findByRole("button", { name: /Set up|设置/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("cancelled at the administrator prompt");
    expect(screen.getByRole("button", { name: /Set up|设置/ })).toBeEnabled();
  });

  it("offers no setup where the sandbox is unavailable for another reason", async () => {
    runtimeMocks.machineSandboxSupport.mockResolvedValue({
      backend: "bubblewrap",
      available: false,
      detail: "bubblewrap is not installed",
      setup: false
    });
    render(<SandboxSettings machine={null} machineName="这台电脑" settings={undefined} onChange={vi.fn()} />);

    expect(await screen.findByText(/bubblewrap is not installed/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Set up|设置/ })).not.toBeInTheDocument();
  });

  it("asks the workspace's own machine, and leaves an SSH machine's setup to that machine", async () => {
    runtimeMocks.machineSandboxSupport.mockResolvedValue({
      ...NEEDS_SETUP,
      detail: "Run `mework-remote.exe sandbox-setup` there as an administrator"
    });
    const machine = { kind: "ssh" as const, machineId: "winbox" };
    render(<SandboxSettings machine={machine} machineName="SSH: winbox" settings={undefined} onChange={vi.fn()} />);

    expect(await screen.findByText(/^SSH: winbox · .*sandbox-setup/)).toBeInTheDocument();
    expect(runtimeMocks.machineSandboxSupport).toHaveBeenCalledWith(machine);
    expect(screen.queryByRole("button", { name: /Set up|设置/ })).not.toBeInTheDocument();
    expect(runtimeMocks.setupLocalSandbox).not.toHaveBeenCalled();
  });
});
