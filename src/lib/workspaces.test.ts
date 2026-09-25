import { describe, expect, it } from "vitest";
import type { AttachedWorkspace, ToolDescriptor } from "../types";
import {
  conversationWorkspaces,
  createTemporaryWorkspace,
  isReservedWorkspace,
  isTemporaryWorkspace,
  machineUsage,
  projectWorkspaces,
  withWorkspaceArgument,
  workspaceDirectoryLabel,
  workspaceEnvKey,
  worktreeFor
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
  // This machine is Windows with PowerShell and Git Bash; the SSH machine has bash.
  const windowsHost = (machine: AttachedWorkspace["machine"]) => machine ? ["bash"] : ["powershell", "bash"];
  const posixHost = (machine: AttachedWorkspace["machine"]) => machine ? ["bash"] : ["zsh", "bash", "sh"];

  it("leaves every descriptor alone while the conversation has one workspace", () => {
    expect(withWorkspaceArgument(tools, [local], windowsHost, "Workspace")).toBe(tools);
  });

  it("offers the host's numbers to the workspace-scoped tools only", () => {
    const [read, bash, powershell, fetch] = withWorkspaceArgument(tools, [local, remote], windowsHost, "Workspace");
    expect(parameterNames(read!)).toEqual(["path", "workspace"]);
    expect(parameterNames(bash!)).toEqual(["path", "workspace"]);
    expect(parameterNames(fetch!)).toEqual(["path"]);
    const argument = read!.parameters.at(-1)!;
    expect(argument).toMatchObject({ type: "number", required: false, placeholder: "1 | 2" });
    expect(argument.defaultValue).toBeUndefined();
    // PowerShell runs only where a machine has it: this one.
    expect(powershell!.parameters.at(-1)).toMatchObject({ name: "workspace", placeholder: "1" });
  });

  it("withdraws the argument from a shell tool where no machine has the shell", () => {
    const [, , powershell] = withWorkspaceArgument(tools, [remote, remote], windowsHost, "Workspace");
    expect(parameterNames(powershell!)).toEqual(["path"]);
    const [, , onPosixHost] = withWorkspaceArgument(tools, [local, remote], posixHost, "Workspace");
    expect(parameterNames(onPosixHost!)).toEqual(["path"]);
  });

  it("does not double a workspace argument a descriptor already declares", () => {
    const declared: ToolDescriptor = {
      ...descriptor("read"),
      parameters: [{ name: "workspace", label: "ws", type: "number", required: false }]
    };
    const [read] = withWorkspaceArgument([declared], [local, remote], windowsHost, "Workspace");
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

  it("lets a legacy worktree record stand in for the first directory only", () => {
    const worktree = { path: "C:\\platform\\.mework\\worktrees\\c1", branch: "b", baseOid: "o" };
    expect(projectWorkspaces(project, { worktrees: [worktree] })[0]).toEqual({ machine: null, path: worktree.path });
    expect(projectWorkspaces(project, { worktrees: [worktree] })[1].path).toBe("/srv/api");
  });

  it("lets each workspace's own worktree stand in for it, on its own machine", () => {
    const registered = projectWorkspaces(project)[1];
    const worktree = {
      path: "/srv/api/.mework/worktrees/conversations/c1",
      branch: "mework/conv/c1",
      baseOid: "abc1234",
      workspace: registered
    };
    const resolved = projectWorkspaces(project, { worktrees: [worktree] });
    expect(resolved[0].path).toBe("C:\\platform");
    expect(resolved[1]).toEqual({ machine: registered.machine, path: worktree.path });
    expect(worktreeFor({ worktrees: [worktree] }, 2, registered)).toBe(worktree);
    // The same path on another machine is another workspace.
    expect(worktreeFor({ worktrees: [worktree] }, 2, { path: "/srv/api" })).toBeNull();
  });

  it("numbers the conversation's attached workspaces after every workspace of the project", () => {
    const numbered = conversationWorkspaces(project, {
      worktrees: [],
      attachedWorkspaces: [{ path: "E:\\notes" }]
    });
    expect(numbered.map((workspace) => workspace.path)).toEqual([
      "C:\\platform", "/srv/api", "D:\\shared\\lib", "E:\\notes"
    ]);
    // A temporary project still takes number 1, for the scratch directory the host gives it.
    expect(conversationWorkspaces(createTemporaryWorkspace(), {
      worktrees: [],
      attachedWorkspaces: [{ path: "E:\\notes" }]
    })).toHaveLength(2);
  });

  it("labels a directory by its last segment on any machine", () => {
    expect(workspaceDirectoryLabel("C:\\platform\\")).toBe("platform");
    expect(workspaceDirectoryLabel("~/code/api")).toBe("api");
    expect(workspaceDirectoryLabel("/")).toBe("/");
  });
});

describe("workspace variables and machine usage", () => {
  it("keys a workspace's variables by its machine and its path", () => {
    expect(workspaceEnvKey(null, "C:\\platform")).toBe("local|C:\\platform");
    expect(workspaceEnvKey({ kind: "wsl", distro: "Ubuntu" }, "/home/dev/app")).toBe("wsl:Ubuntu|/home/dev/app");
    // The same spelling on two machines is two workspaces, with two tables.
    expect(workspaceEnvKey({ kind: "ssh", machineId: "m1" }, "/srv/app"))
      .not.toBe(workspaceEnvKey({ kind: "ssh", machineId: "m2" }, "/srv/app"));
  });

  it("counts projects by any of their workspaces and conversations by their attached ones", () => {
    const devbox = { kind: "ssh" as const, machineId: "m1" };
    const base = { ...createTemporaryWorkspace(), kind: "directory" as const, conversations: [] };
    const workspaces = [
      { ...base, id: "a", path: "/srv/a", machine: devbox },
      { ...base, id: "b", path: "C:\\b", additionalWorkspaces: [{ machine: devbox, path: "/srv/b" }] },
      {
        ...base,
        id: "c",
        path: "C:\\c",
        conversations: [
          { attachedWorkspaces: [{ machine: devbox, path: "~/x" }] },
          { attachedWorkspaces: [{ path: "D:\\y" }] }
        ] as unknown as typeof base.conversations
      },
      // The temporary project has no directory of its own and never counts as on this machine.
      createTemporaryWorkspace()
    ];
    expect(machineUsage(workspaces, devbox)).toEqual({ projects: 2, conversations: 1 });
    expect(machineUsage(workspaces, null)).toEqual({ projects: 2, conversations: 1 });
    expect(machineUsage(workspaces, { kind: "ssh", machineId: "gone" })).toEqual({ projects: 0, conversations: 0 });
    expect(machineUsage(null, null)).toEqual({ projects: 0, conversations: 0 });
  });
});
