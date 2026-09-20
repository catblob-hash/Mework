import { describe, expect, it } from "vitest";
import {
  createTemporaryWorkspace,
  isReservedWorkspace,
  isTemporaryWorkspace
} from "./workspaces";

describe("workspace modes", () => {
  it("treats only the canonical temporary workspace as reserved", () => {
    expect(isReservedWorkspace(createTemporaryWorkspace())).toBe(true);
    expect(isReservedWorkspace({
      ...createTemporaryWorkspace(),
      id: "ws_mework",
      kind: "directory",
      path: "C:\\test\\Mework"
    })).toBe(false);
  });

  it("creates a canonical temporary workspace", () => {
    const workspace = createTemporaryWorkspace();
    expect(workspace).toMatchObject({
      id: "__temporary__",
      name: "临时工作区",
      kind: "temporary",
      path: "",
      conversations: []
    });
    expect(isTemporaryWorkspace(workspace)).toBe(true);
  });
});
