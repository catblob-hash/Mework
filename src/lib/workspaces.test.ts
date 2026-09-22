import { describe, expect, it } from "vitest";
import type { AttachedWorkspace, ToolDescriptor } from "../types";
import {
  createTemporaryWorkspace,
  isReservedWorkspace,
  isTemporaryWorkspace,
  withWorkspaceArgument
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

describe("withWorkspaceArgument", () => {
  const descriptor = (name: string, category: ToolDescriptor["category"] = "filesystem"): ToolDescriptor => ({
    name,
    label: name,
    description: "",
    category,
    dangerous: false,
    parameters: [{ name: "path", label: "path", type: "string", required: true }]
  });
  const tools = [
    descriptor("read"),
    descriptor("bash", "shell"),
    descriptor("powershell", "shell"),
    descriptor("web_fetch", "web")
  ];
  const local: AttachedWorkspace = { machine: null, path: "C:\\src\\app" };
  const remote: AttachedWorkspace = { machine: { kind: "ssh", machineId: "m1" }, path: "/srv/app" };
  const parameterNames = (tool: ToolDescriptor) => tool.parameters.map((parameter) => parameter.name);

  it("leaves every descriptor alone while the conversation has one workspace", () => {
    expect(withWorkspaceArgument(tools, [local], true, "Workspace")).toBe(tools);
  });

  it("offers the host's numbers to the workspace-scoped tools only", () => {
    const [read, bash, powershell, fetch] = withWorkspaceArgument(tools, [local, remote], true, "Workspace");
    expect(parameterNames(read!)).toEqual(["path", "workspace"]);
    expect(parameterNames(bash!)).toEqual(["path", "workspace"]);
    expect(parameterNames(fetch!)).toEqual(["path"]);
    const argument = read!.parameters.at(-1)!;
    expect(argument).toMatchObject({ type: "number", required: false, placeholder: "1 | 2" });
    expect(argument.defaultValue).toBeUndefined();
    // PowerShell runs only on this machine's workspaces.
    expect(powershell!.parameters.at(-1)).toMatchObject({ name: "workspace", placeholder: "1" });
  });

  it("withdraws the argument from powershell where nothing runs it", () => {
    const [, , powershell] = withWorkspaceArgument(tools, [remote, remote], true, "Workspace");
    expect(parameterNames(powershell!)).toEqual(["path"]);
    const [, , onPosixHost] = withWorkspaceArgument(tools, [local, remote], false, "Workspace");
    expect(parameterNames(onPosixHost!)).toEqual(["path"]);
  });

  it("does not double a workspace argument a descriptor already declares", () => {
    const declared: ToolDescriptor = {
      ...descriptor("read"),
      parameters: [{ name: "workspace", label: "ws", type: "number", required: false }]
    };
    const [read] = withWorkspaceArgument([declared], [local, remote], true, "Workspace");
    expect(read).toBe(declared);
  });
});
