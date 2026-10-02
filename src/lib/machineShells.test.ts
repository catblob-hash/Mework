import { describe, expect, it } from "vitest";
import type { ExecutionEnvironmentAssets, MachineShells } from "../types";
import {
  availableShellBackends,
  backendOfTool,
  effectiveAgentShell,
  knownShells,
  preferredBackend,
  SHELL_BACKENDS_BY_OS,
  sshEndpoint,
  terminalShellsFor,
  toolsForShellBackends,
  withAgentShell,
  withDefaultAgentShell,
  withoutProbe
} from "./machineShells";

const probe = (os: MachineShells["os"], backends: MachineShells["shells"][number]["backend"][]): MachineShells => ({
  os,
  shells: backends.map((backend) => ({ backend, path: `/bin/${backend}` })),
  probedAt: "2026-09-23T00:00:00Z"
});

const assets = (overrides: Partial<ExecutionEnvironmentAssets> = {}): ExecutionEnvironmentAssets => ({
  sshMachines: [{
    id: "m1",
    name: "devbox",
    host: "dev@devbox",
    port: 0,
    identityFile: "",
    createdAt: "",
    updatedAt: ""
  }],
  envVars: {},
  ...overrides
});

describe("the shell × OS table", () => {
  it("registers no shell that cannot carry Mework's own scripts", () => {
    expect(SHELL_BACKENDS_BY_OS.windows).toEqual(["powershell", "bash"]);
    expect(SHELL_BACKENDS_BY_OS.wsl).not.toContain("powershell");
    expect(SHELL_BACKENDS_BY_OS.macos).not.toContain("powershell");
  });

  it("maps every shell tool back to its backend", () => {
    expect(backendOfTool("zsh")).toBe("zsh");
    expect(backendOfTool("powershell")).toBe("powershell");
    expect(backendOfTool("read")).toBeNull();
    expect(backendOfTool("shell")).toBeNull();
  });

  it("ranks each OS's shells in Mework's own fixed order", () => {
    expect(SHELL_BACKENDS_BY_OS.windows).toEqual(["powershell", "bash"]);
    expect(SHELL_BACKENDS_BY_OS.linux).toEqual(["bash", "zsh", "sh"]);
    expect(SHELL_BACKENDS_BY_OS.macos).toEqual(["zsh", "bash", "sh"]);
    expect(preferredBackend("windows", ["bash", "powershell"])).toBe("powershell");
    expect(preferredBackend("windows", ["bash"])).toBe("bash");
    expect(preferredBackend("macos", ["sh", "bash"])).toBe("bash");
    expect(preferredBackend("macos", [])).toBeNull();
  });
});

describe("what a conversation's machines offer", () => {
  const windows = { machine: { kind: "ssh" as const, machineId: "m1" }, path: "C:/work" };
  const local = { machine: null, path: "/Users/dev/app" };

  it("lists the union of the machines' probed backends", () => {
    const probes = { local: probe("macos", ["zsh", "bash", "sh"]), "ssh:m1": probe("windows", ["powershell"]) };
    expect(availableShellBackends([local, windows], probes, "MacIntel")).toEqual(["bash", "zsh", "sh", "powershell"]);
    expect(availableShellBackends([windows], probes, "MacIntel")).toEqual(["powershell"]);
    const tools = ["read", "bash", "powershell", "zsh"].map((name) => ({ name }));
    expect(toolsForShellBackends(tools, ["powershell"]).map((tool) => tool.name)).toEqual(["read", "powershell"]);
  });

  it("assumes bash on a remote machine nobody has probed, and the host OS's shells before the host's probe", () => {
    expect(knownShells(windows.machine, {}, "Win32")).toEqual({ os: null, backends: ["bash"] });
    expect(knownShells({ kind: "wsl", distro: "Ubuntu" }, {}, "Win32").os).toBe("wsl");
    expect(knownShells(null, {}, "Win32").backends).toEqual(["powershell", "bash"]);
    expect(knownShells(null, {}, "MacIntel").backends).toEqual(["zsh", "bash", "sh"]);
    // An unknown platform keeps every tool rather than hiding one the host would run.
    expect(knownShells(null, {}, "").backends).toEqual(["bash", "zsh", "sh", "powershell"]);
    // No workspace at all is this machine.
    expect(availableShellBackends([], {}, "Linux x86_64")).toEqual(["bash", "zsh", "sh"]);
  });
});

describe("the shells a terminal offers", () => {
  const wsl = { kind: "wsl" as const, distro: "Ubuntu" };
  const ssh = { kind: "ssh" as const, machineId: "m1" };

  it("lists the machine's probed shells in its OS's priority order", () => {
    const probes = { local: probe("macos", ["zsh", "bash", "sh"]), "wsl:Ubuntu": probe("wsl", ["bash", "sh"]) };
    // This machine's sh is left out: its line editor cannot hold a line for the Git mutex.
    expect(terminalShellsFor(null, probes, "MacIntel")).toEqual(["zsh", "bash"]);
    // A shell the probe did not find is not offered, however high it ranks.
    expect(terminalShellsFor(wsl, probes, "Win32")).toEqual(["bash", "sh"]);
  });

  it("offers PowerShell on a Windows host and a Windows SSH machine, never in WSL", () => {
    const probes = {
      local: probe("windows", ["powershell", "bash"]),
      "ssh:m1": probe("windows", ["powershell", "bash"]),
      "wsl:Ubuntu": probe("wsl", ["bash"])
    };
    expect(terminalShellsFor(null, probes, "Win32")).toEqual(["powershell", "bash"]);
    expect(terminalShellsFor(ssh, probes, "Win32")).toEqual(["powershell", "bash"]);
    expect(terminalShellsFor(wsl, probes, "Win32")).toEqual(["bash"]);
  });

  it("falls back to what is assumed of a machine nobody has probed", () => {
    expect(terminalShellsFor(null, {}, "Win32")).toEqual(["powershell", "bash"]);
    expect(terminalShellsFor(undefined, {}, "Linux x86_64")).toEqual(["bash", "zsh"]);
    expect(terminalShellsFor(ssh, {}, "MacIntel")).toEqual(["bash"]);
  });

  it("offers nothing when the probe found nothing a terminal can start", () => {
    expect(terminalShellsFor(ssh, { "ssh:m1": probe("linux", []) }, "MacIntel")).toEqual([]);
  });
});

describe("agent shells", () => {
  it("records a new machine's first probe as its OS's most preferred backend it has", () => {
    const next = withDefaultAgentShell(assets(), "ssh:m1", probe("linux", ["sh", "zsh", "bash"]));
    expect(next.sshMachines[0]!.agentShell).toBe("bash");
    // A machine that already has a choice keeps it.
    expect(withDefaultAgentShell(next, "ssh:m1", probe("linux", ["bash"]))).toBe(next);
    // This machine has no agent shell, and a deleted machine has nothing to record.
    const untouched = assets();
    expect(withDefaultAgentShell(untouched, "local", probe("macos", ["zsh"]))).toBe(untouched);
    expect(withDefaultAgentShell(untouched, "ssh:gone", probe("linux", ["bash"]))).toBe(untouched);
  });

  it("records a WSL distribution's choice by name", () => {
    const next = withDefaultAgentShell(assets(), "wsl:Ubuntu", probe("wsl", ["bash", "sh"]));
    expect(next.wslAgentShells).toEqual({ Ubuntu: "bash" });
    expect(withAgentShell(next, { kind: "wsl", distro: "Ubuntu" }, "sh").wslAgentShells).toEqual({ Ubuntu: "sh" });
  });

  it("resolves a choice the machine no longer has the way the host does", () => {
    const chosen = withAgentShell(assets(), { kind: "ssh", machineId: "m1" }, "zsh");
    const machine = { kind: "ssh" as const, machineId: "m1" };
    expect(effectiveAgentShell(machine, chosen, { "ssh:m1": probe("linux", ["zsh", "bash"]) })).toBe("zsh");
    expect(effectiveAgentShell(machine, chosen, { "ssh:m1": probe("linux", ["bash", "sh"]) })).toBe("bash");
    expect(effectiveAgentShell(machine, chosen, {})).toBe("zsh");
    expect(effectiveAgentShell(null, chosen, {})).toBeNull();
  });
});

describe("a machine moved to another address", () => {
  const machine = { kind: "ssh" as const, machineId: "m1" };
  const moved = (host: string) => {
    const base = assets();
    return { ...base, sshMachines: base.sshMachines.map((entry) => ({ ...entry, host })) };
  };

  it("is told apart by its endpoint, not its id", () => {
    const before = sshEndpoint(assets(), machine);
    expect(before).not.toBeNull();
    expect(sshEndpoint(assets(), machine)).toBe(before);
    expect(sshEndpoint(moved("dev@other"), machine)).not.toBe(before);
    expect(sshEndpoint({ ...assets(), sshMachines: [] }, machine)).toBeNull();
    expect(sshEndpoint(assets(), { kind: "wsl", distro: "Ubuntu" })).toBeNull();
    expect(sshEndpoint(assets(), null)).toBeNull();
  });

  it("loses its old probe, so it reads as never probed until the new address answers", () => {
    const probes = { local: probe("macos", ["zsh"]), "ssh:m1": probe("linux", ["bash"]) };
    const next = withoutProbe(probes, "ssh:m1");
    expect(next).toEqual({ local: probes.local });
    expect(knownShells(machine, next, "MacIntel")).toEqual({ os: null, backends: ["bash"] });
    expect(withoutProbe(next, "ssh:m1")).toBe(next);
  });
});
