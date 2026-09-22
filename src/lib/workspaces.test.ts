import { describe, expect, it } from "vitest";
import type { AttachedWorkspace, ToolDescriptor } from "../types";
import {
  conversationWorkspaces,
  createTemporaryWorkspace,
  hostLacksPowerShell,
  isReservedWorkspace,
  isTemporaryWorkspace,
  projectWorkspaces,
  terminalShellsFor,
  toolsForHost,
  withWorkspaceArgument,
  workspaceDirectoryLabel
} from "./workspaces";

describe("PowerShell on the host", () => {
  const names = (platform: string) => toolsForHost(
    ["read", "bash", "powershell", "powershell_find_output", "bash_find_output"].map((name) => ({ name })),
    platform
  ).map((tool) => tool.name);

  it("withdraws both PowerShell tools on a Mac or Linux host", () => {
    for (const platform of ["MacIntel", "Linux x86_64"]) {
      expect(hostLacksPowerShell(platform)).toBe(true);
      expect(names(platform)).toEqual(["read", "bash", "bash_find_output"]);
    }
  });

  it("keeps them on Windows and when the platform is unknown", () => {
    for (const platform of ["Win32", ""]) {
      expect(hostLacksPowerShell(platform)).toBe(false);
      expect(names(platform)).toContain("powershell");
      expect(names(platform)).toContain("powershell_find_output");
    }
  });
});

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

describe("project workspaces", () => {
  const project = {
    ...createTemporaryWorkspace(),
    id: "ws_platform",
    kind: "directory" as const,
    path: "C:\\platform",
    additionalWorkspaces: [
      { machine: { kind: "ssh" as const, machineId: "m1" }, path: "/srv/api" },
      { path: "D:\\shared\\lib" }
    ]
  };

  it("lists the project's first directory and then the ones added after it", () => {
    expect(projectWorkspaces(project)).toEqual([
      { machine: null, path: "C:\\platform" },
      { machine: { kind: "ssh", machineId: "m1" }, path: "/srv/api" },
      { path: "D:\\shared\\lib" }
    ]);
    expect(projectWorkspaces(createTemporaryWorkspace())).toEqual([]);
  });

  it("lets a conversation's worktree stand in for the first directory only", () => {
    const worktree = { path: "C:\\platform\\.mework\\worktrees\\c1", branch: "b", baseOid: "o" };
    expect(projectWorkspaces(project, { worktree })[0]).toEqual({ machine: null, path: worktree.path });
    expect(projectWorkspaces(project, { worktree })[1].path).toBe("/srv/api");
  });

  it("numbers the conversation's attached workspaces after every workspace of the project", () => {
    const numbered = conversationWorkspaces(project, {
      worktree: null,
      attachedWorkspaces: [{ path: "E:\\notes" }]
    });
    expect(numbered.map((workspace) => workspace.path)).toEqual([
      "C:\\platform", "/srv/api", "D:\\shared\\lib", "E:\\notes"
    ]);
    // A temporary project still takes number 1, for the scratch directory the host gives it.
    expect(conversationWorkspaces(createTemporaryWorkspace(), {
      worktree: null,
      attachedWorkspaces: [{ path: "E:\\notes" }]
    })).toHaveLength(2);
  });

  it("labels a directory by its last segment on any machine", () => {
    expect(workspaceDirectoryLabel("C:\\platform\\")).toBe("platform");
    expect(workspaceDirectoryLabel("~/code/api")).toBe("api");
    expect(workspaceDirectoryLabel("/")).toBe("/");
  });
});

describe("terminalShellsFor", () => {
  it("offers PowerShell and Git Bash only for a directory on a Windows host", () => {
    expect(terminalShellsFor(null, "Win32")).toEqual(["powershell", "bash"]);
    expect(terminalShellsFor({ kind: "wsl", distro: "Ubuntu" }, "Win32")).toEqual(["zsh", "bash", "fish"]);
    expect(terminalShellsFor({ kind: "ssh", machineId: "m1" }, "Win32")).toEqual(["zsh", "bash", "fish"]);
  });

  it("offers the POSIX shells on a Mac or Linux host", () => {
    expect(terminalShellsFor(null, "MacIntel")).toEqual(["zsh", "bash", "fish"]);
    expect(terminalShellsFor(undefined, "Linux x86_64")).toEqual(["zsh", "bash", "fish"]);
  });
});
