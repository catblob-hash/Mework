import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SandboxSupport } from "../../types";
import { SandboxSettings } from ".";

const runtimeMocks = vi.hoisted(() => ({
  localSandboxSupport: vi.fn(),
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
    runtimeMocks.localSandboxSupport.mockReset();
    runtimeMocks.setupLocalSandbox.mockReset();
  });

  it("offers the one-time Windows setup and shows the sandbox available after it", async () => {
    runtimeMocks.localSandboxSupport.mockResolvedValue(NEEDS_SETUP);
    runtimeMocks.setupLocalSandbox.mockResolvedValue({ backend: "srt-win", available: true, detail: "", setup: false });
    render(<SandboxSettings settings={undefined} onChange={vi.fn()} />);

    await userEvent.click(await screen.findByRole("button", { name: /Set up|设置/ }));

    expect(runtimeMocks.setupLocalSandbox).toHaveBeenCalledTimes(1);
    expect(await screen.findByText(/Windows sandbox$|Windows 沙箱$/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Set up|设置/ })).not.toBeInTheDocument();
  });

  it("keeps the setup offered and says why when it fails", async () => {
    runtimeMocks.localSandboxSupport.mockResolvedValue(NEEDS_SETUP);
    runtimeMocks.setupLocalSandbox.mockRejectedValue("The setup was cancelled at the administrator prompt");
    render(<SandboxSettings settings={undefined} onChange={vi.fn()} />);

    await userEvent.click(await screen.findByRole("button", { name: /Set up|设置/ }));

    expect(await screen.findByRole("alert")).toHaveTextContent("cancelled at the administrator prompt");
    expect(screen.getByRole("button", { name: /Set up|设置/ })).toBeEnabled();
  });

  it("offers no setup where the sandbox is unavailable for another reason", async () => {
    runtimeMocks.localSandboxSupport.mockResolvedValue({
      backend: "bubblewrap",
      available: false,
      detail: "bubblewrap is not installed",
      setup: false
    });
    render(<SandboxSettings settings={undefined} onChange={vi.fn()} />);

    expect(await screen.findByText(/bubblewrap is not installed/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Set up|设置/ })).not.toBeInTheDocument();
  });
});
