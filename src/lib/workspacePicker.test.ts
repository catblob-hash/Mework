import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@tauri-apps/api/core")>()),
  invoke: tauriMocks.invoke
}));

import { pickWorkspaceDirectory } from "./workspacePicker";

describe("workspace picker IPC domains", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    tauriMocks.invoke.mockReset();
  });

  afterEach(() => {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  });

  it("uses the backend workspace authorization command", async () => {
    tauriMocks.invoke.mockResolvedValueOnce("C:/workspace");
    await expect(pickWorkspaceDirectory()).resolves.toBe("C:/workspace");
    expect(tauriMocks.invoke).toHaveBeenCalledWith("pick_workspace_directory");
  });

  it("never transmits a renderer-selected path to the workspace authorization command", async () => {
    tauriMocks.invoke.mockResolvedValueOnce("C:/host-issued-fixture");

    await expect(pickWorkspaceDirectory()).resolves.toBe("C:/host-issued-fixture");
    expect(tauriMocks.invoke.mock.calls).toEqual([["pick_workspace_directory"]]);
  });
});
