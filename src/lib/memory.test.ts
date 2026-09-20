import { beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => ({
  invoke: vi.fn()
}));

vi.mock("./backend", () => backend);

import {
  deleteMemoryDocument,
  getMemoryOverview,
  readMemoryFile,
  writeMemoryFile
} from "./memory";

beforeEach(() => {
  backend.invoke.mockReset();
});

describe("memory file adapters", () => {
  it("addresses a document by tier and name, never by path", async () => {
    // The backend resolves ~/.mework and <workspace>/.mework itself. If the
    // renderer could send a path, a compromised renderer could read or write
    // anywhere on disk; keeping this surface path-free is the whole boundary.
    backend.invoke.mockResolvedValue({
      tier: "project",
      name: "build.md",
      path: "C:\\work\\.mework\\memory\\build.md",
      content: "测试要用项目自带环境"
    });

    await expect(readMemoryFile("ws_1", "project", "build.md")).resolves.toMatchObject({
      name: "build.md",
      content: "测试要用项目自带环境"
    });
    expect(backend.invoke).toHaveBeenCalledWith("mework_memory_read_file", {
      workspaceId: "ws_1",
      tier: "project",
      name: "build.md"
    });
    for (const [, payload] of backend.invoke.mock.calls) {
      expect(Object.keys(payload)).not.toContain("path");
    }
  });

  it("passes a null workspace through rather than inventing one", async () => {
    // A conversation with no directory-backed workspace has no project tier.
    // Sending null lets the backend say so, instead of silently resolving
    // whichever workspace the renderer happened to remember last.
    backend.invoke.mockResolvedValue({
      global: {
        tier: "global",
        available: true,
        rootPath: "C:\\home\\.mework",
        instructions: null,
        index: null,
        documents: []
      },
      project: {
        tier: "project",
        available: false,
        rootPath: null,
        instructions: null,
        index: null,
        documents: []
      }
    });

    const overview = await getMemoryOverview(null);
    expect(overview.project.available).toBe(false);
    expect(backend.invoke).toHaveBeenCalledWith("mework_memory_overview", { workspaceId: null });

    backend.invoke.mockClear();
    await getMemoryOverview("");
    expect(backend.invoke).toHaveBeenCalledWith("mework_memory_overview", { workspaceId: null });
  });

  it("sends content only on write and resolves delete to nothing", async () => {
    backend.invoke.mockResolvedValue({
      tier: "global",
      name: "preferences.md",
      path: "C:\\home\\.mework\\memory\\preferences.md",
      content: "新正文"
    });
    await writeMemoryFile("ws_1", "global", "preferences.md", "新正文");
    expect(backend.invoke).toHaveBeenCalledWith("mework_memory_write_file", {
      workspaceId: "ws_1",
      tier: "global",
      name: "preferences.md",
      content: "新正文"
    });

    backend.invoke.mockReset();
    backend.invoke.mockResolvedValue({ unexpected: "payload" });
    await expect(deleteMemoryDocument("ws_1", "global", "stale.md")).resolves.toBeUndefined();
    expect(backend.invoke).toHaveBeenCalledWith("mework_memory_delete_document", {
      workspaceId: "ws_1",
      tier: "global",
      name: "stale.md"
    });
  });

  it("surfaces a backend rejection instead of resolving to an empty document", async () => {
    backend.invoke.mockRejectedValue(new Error("全局记忆中没有名为 absent.md 的记忆文档"));
    await expect(readMemoryFile(null, "global", "absent")).rejects.toThrow("absent.md");
  });

  it("carries no model identity, scope, or version anywhere in its surface", async () => {
    // Memory belongs to a location. The retired adapters took a modelId, a
    // scope selector and a CAS version; none of those exist any more, and a
    // reappearance would mean per-model namespacing had come back.
    backend.invoke.mockResolvedValue({ tier: "global", name: "a.md", path: "p", content: "c" });
    await readMemoryFile("ws_1", "global", "a");
    await writeMemoryFile("ws_1", "global", "a", "c");
    await deleteMemoryDocument("ws_1", "global", "a");
    for (const [command, payload] of backend.invoke.mock.calls) {
      expect(command.startsWith("mework_memory_")).toBe(true);
      for (const retired of ["modelId", "model_id", "scope", "version", "expectedVersion"]) {
        expect(Object.keys(payload)).not.toContain(retired);
      }
    }
  });
});
