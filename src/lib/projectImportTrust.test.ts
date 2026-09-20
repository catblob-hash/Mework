import { beforeEach, describe, expect, it, vi } from "vitest";

const backendMocks = vi.hoisted(() => ({
  invoke: vi.fn()
}));

vi.mock("./backend", () => backendMocks);

import {
  listProjectImportTrust,
  revokeProjectImportTrust
} from "./projectImportTrust";

beforeEach(() => {
  backendMocks.invoke.mockReset();
});

describe("project import trust command adapters", () => {
  it("keeps canonical paths and contents out of renderer-facing summaries", async () => {
    backendMocks.invoke.mockResolvedValue([{
      id: "42b1bf74-fb45-4387-909f-240058691a60",
      workspaceId: "workspace-1",
      label: "shared-rules",
      targetKind: "directory",
      decision: "denied",
      activeForCurrentWorkspace: true,
      createdAt: "2026-07-24T08:00:00Z",
      updatedAt: "2026-07-24T08:00:00Z",
      canonicalPath: "C:/Users/private/shared-rules",
      canonicalWorkspace: "C:/Users/private/Mework",
      content: "PRIVATE PROJECT INSTRUCTIONS"
    }]);

    const records = await listProjectImportTrust("workspace-1");

    expect(backendMocks.invoke).toHaveBeenCalledWith(
      "project_memory_list_import_trust",
      { workspaceId: "workspace-1" }
    );
    expect(records).toEqual([{
      id: "42b1bf74-fb45-4387-909f-240058691a60",
      workspaceId: "workspace-1",
      label: "shared-rules",
      targetKind: "directory",
      decision: "denied",
      activeForCurrentWorkspace: true,
      createdAt: "2026-07-24T08:00:00Z",
      updatedAt: "2026-07-24T08:00:00Z"
    }]);
    expect(JSON.stringify(records)).not.toContain("C:/Users/private");
    expect(JSON.stringify(records)).not.toContain("PRIVATE PROJECT INSTRUCTIONS");
  });

  it("fails closed on cross-workspace or malformed metadata", async () => {
    backendMocks.invoke.mockResolvedValue([{
      id: "record-1",
      workspaceId: "workspace-2",
      label: "outside.md",
      targetKind: "file",
      decision: "allowed",
      activeForCurrentWorkspace: true,
      createdAt: "2026-07-24T08:00:00Z",
      updatedAt: "2026-07-24T08:00:00Z"
    }]);

    await expect(listProjectImportTrust("workspace-1")).rejects.toThrow(
      "unsafe metadata"
    );
  });

  it("revokes only the exact workspace-owned record", async () => {
    backendMocks.invoke.mockResolvedValue(undefined);

    await revokeProjectImportTrust(
      "workspace-1",
      "42b1bf74-fb45-4387-909f-240058691a60"
    );

    expect(backendMocks.invoke).toHaveBeenCalledWith(
      "project_memory_revoke_import_trust",
      {
        workspaceId: "workspace-1",
        recordId: "42b1bf74-fb45-4387-909f-240058691a60"
      }
    );
  });

  it("never forwards private backend diagnostics into renderer errors", async () => {
    backendMocks.invoke.mockRejectedValue(
      new Error("C:/Users/private/Mework: PRIVATE PROJECT INSTRUCTIONS")
    );

    const listError = await listProjectImportTrust("workspace-1").catch((error) => error);
    const revokeError = await revokeProjectImportTrust(
      "workspace-1",
      "42b1bf74-fb45-4387-909f-240058691a60"
    ).catch((error) => error);

    expect(String(listError)).toContain("Project import trust list failed");
    expect(String(revokeError)).toContain("Project import trust revoke failed");
    expect(`${String(listError)} ${String(revokeError)}`).not.toMatch(
      /C:\/Users\/private|PRIVATE PROJECT/
    );
  });
});
