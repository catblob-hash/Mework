import { invoke } from "./backend";
import type {
  AttachedWorkspace,
  ExecutionEnvironmentAssets,
  MachineOs,
  MachineShells,
  RunTarget,
  ShellBackend,
  SshMachineConfig
} from "../types";
import { runEnvKey } from "./workspaces";
import type { TerminalShell } from "./workspaces";

/**
 * Shell backends per operating system, most preferred first. Mirrors
 * `shell_backend::backends_for`: a combination is listed only when Mework can
 * run the shell tool *and* its own file-tool and language-server scripts
 * through it, so a shell absent here is never probed on that OS. The order is
 * Mework's own and the only one: nothing asks the user to rank shells.
 */
export const SHELL_BACKENDS_BY_OS: Record<MachineOs, readonly ShellBackend[]> = {
  windows: ["powershell", "bash"],
  macos: ["zsh", "bash", "sh"],
  linux: ["bash", "zsh", "sh"],
  wsl: ["bash", "zsh", "sh"]
};

/** Every backend, in the order its tools are listed. Mirrors `ShellBackend::ALL`. */
export const SHELL_BACKENDS: readonly ShellBackend[] = ["bash", "zsh", "sh", "powershell"];

export function isShellBackend(value: unknown): value is ShellBackend {
  return typeof value === "string" && (SHELL_BACKENDS as readonly string[]).includes(value);
}

export function shellBackendLabel(backend: ShellBackend): string {
  switch (backend) {
    case "bash": return "Bash";
    case "zsh": return "zsh";
    case "sh": return "sh";
    case "powershell": return "PowerShell";
  }
}

export function machineOsLabel(os: MachineOs): string {
  switch (os) {
    case "windows": return "Windows";
    case "macos": return "macOS";
    case "linux": return "Linux";
    case "wsl": return "WSL";
  }
}

/** The backend a shell tool runs in, or `null` for any other tool. Mirrors `ShellBackend::of_tool`. */
export function backendOfTool(toolName: string): ShellBackend | null {
  return SHELL_BACKENDS.find((backend) => backend === toolName) ?? null;
}

export function isRegistered(os: MachineOs, backend: ShellBackend): boolean {
  return SHELL_BACKENDS_BY_OS[os].includes(backend);
}

/**
 * The first backend in `os`'s priority order that `available` has: the agent
 * shell a newly probed machine starts with, and the shell tool a fresh
 * install's presets turn on. Mirrors `shell_backend::preferred_backend`.
 */
export function preferredBackend(os: MachineOs, available: readonly ShellBackend[]): ShellBackend | null {
  return SHELL_BACKENDS_BY_OS[os].find((backend) => available.includes(backend)) ?? null;
}

/**
 * The OS the renderer's own platform string names, or `null` when it names
 * none this table knows.
 */
function hostMachineOs(platform: string): MachineOs | null {
  const value = platform.trim();
  if (/^win/i.test(value)) return "windows";
  if (/^(mac|iphone|ipad)/i.test(value)) return "macos";
  if (/^linux/i.test(value)) return "linux";
  return null;
}

/**
 * A machine's OS and shells as far as they are known: its last probe, or — for
 * a remote machine never probed — bash alone, which is what every remote leg
 * ran before machines had backends. Mirrors `machine_shells::known`.
 *
 * This machine is probed at startup, and until that answer arrives its OS's
 * registered backends stand in, from the platform string; an unrecognized
 * platform keeps every backend, because hiding a tool the host would run is
 * the worse mistake.
 */
export function knownShells(
  machine: RunTarget | null | undefined,
  probes: Readonly<Record<string, MachineShells>>,
  platform: string
): { os: MachineOs | null; backends: ShellBackend[] } {
  const probed = probes[runEnvKey(machine ?? null)];
  if (probed) return { os: probed.os, backends: probed.shells.map((shell) => shell.backend) };
  if (!machine) {
    const os = hostMachineOs(platform);
    return { os, backends: os ? [...SHELL_BACKENDS_BY_OS[os]] : [...SHELL_BACKENDS] };
  }
  return { os: machine.kind === "wsl" ? "wsl" : null, backends: ["bash"] };
}

/**
 * The shell backends the tool list offers for a set of workspaces: the union of
 * their machines' backends, in tool order. No workspace at all is this
 * machine, as the host resolves it.
 */
export function availableShellBackends(
  workspaces: readonly AttachedWorkspace[],
  probes: Readonly<Record<string, MachineShells>>,
  platform: string
): ShellBackend[] {
  const found = new Set<ShellBackend>();
  const machines = workspaces.length ? workspaces.map((workspace) => workspace.machine ?? null) : [null];
  for (const machine of machines) {
    for (const backend of knownShells(machine, probes, platform).backends) found.add(backend);
  }
  return SHELL_BACKENDS.filter((backend) => found.has(backend));
}

/**
 * Whether a terminal on `machine` can start `backend`. Mirrors the host's
 * `terminal::TerminalLaunch`: this machine refuses `sh`, whose line editor
 * cannot hold a line for the Git mutex; a WSL distribution has no PowerShell;
 * an SSH machine runs whatever it has, PowerShell being its agent's own default
 * on Windows.
 */
function terminalCanStart(machine: RunTarget | null, backend: ShellBackend): boolean {
  if (!machine) return backend !== "sh";
  if (machine.kind === "wsl") return backend !== "powershell";
  return true;
}

/**
 * The shells a terminal in a workspace on `machine` can start, most preferred
 * first: the ones its probe found, in its OS's priority order, that a terminal
 * there can run. The first is the one a terminal nobody chose a shell for
 * starts. Empty when the probe found none of them.
 */
export function terminalShellsFor(
  machine: RunTarget | null | undefined,
  probes: Readonly<Record<string, MachineShells>>,
  platform: string
): TerminalShell[] {
  const target = machine ?? null;
  const { os, backends } = knownShells(target, probes, platform);
  const ranked = os ? SHELL_BACKENDS_BY_OS[os] : SHELL_BACKENDS;
  return ranked.filter((backend) => backends.includes(backend) && terminalCanStart(target, backend));
}

/** The catalog with every shell tool whose backend is not in `backends` removed. */
export function toolsForShellBackends<T extends { name: string }>(
  tools: readonly T[],
  backends: readonly ShellBackend[]
): T[] {
  return tools.filter((tool) => {
    const backend = backendOfTool(tool.name);
    return backend === null || backends.includes(backend);
  });
}

/**
 * The agent shell a machine's scripts run in, as the host resolves it: the one
 * its settings chose when the machine still has it, else the first of its OS's
 * priority order the probe found. `null` for this machine, which has none.
 */
export function effectiveAgentShell(
  machine: RunTarget | null,
  assets: ExecutionEnvironmentAssets,
  probes: Readonly<Record<string, MachineShells>>
): ShellBackend | null {
  if (!machine) return null;
  const configured = configuredAgentShell(machine, assets);
  const probed = probes[runEnvKey(machine)];
  if (!probed) return configured ?? "bash";
  const available = probed.shells.map((shell) => shell.backend);
  if (configured && available.includes(configured)) return configured;
  return preferredBackend(probed.os, available) ?? "bash";
}

/** The agent shell recorded in a machine's settings, if any. */
function configuredAgentShell(
  machine: RunTarget,
  assets: ExecutionEnvironmentAssets
): ShellBackend | null {
  if (machine.kind === "wsl") return assets.wslAgentShells?.[machine.distro] ?? null;
  return assets.sshMachines.find((entry) => entry.id === machine.machineId)?.agentShell ?? null;
}

/**
 * The execution environments with `machine`'s agent shell set to `backend`.
 * Returns the input unchanged when nothing would change.
 */
export function withAgentShell(
  assets: ExecutionEnvironmentAssets,
  machine: RunTarget,
  backend: ShellBackend
): ExecutionEnvironmentAssets {
  if (configuredAgentShell(machine, assets) === backend) return assets;
  if (machine.kind === "wsl") {
    return {
      ...assets,
      wslAgentShells: { ...(assets.wslAgentShells ?? {}), [machine.distro]: backend }
    };
  }
  return {
    ...assets,
    sshMachines: assets.sshMachines.map((entry): SshMachineConfig => entry.id === machine.machineId
      ? { ...entry, agentShell: backend, updatedAt: new Date().toISOString() }
      : entry)
  };
}

/**
 * Records a machine's agent shell the first time it is probed: the first
 * backend of its OS's priority order that it has. A machine that already has a
 * choice keeps it.
 */
export function withDefaultAgentShell(
  assets: ExecutionEnvironmentAssets,
  key: string,
  shells: MachineShells
): ExecutionEnvironmentAssets {
  const machine = machineOfEnvKey(key);
  if (!machine) return assets;
  if (machine.kind === "ssh" && !assets.sshMachines.some((entry) => entry.id === machine.machineId)) {
    return assets;
  }
  if (configuredAgentShell(machine, assets)) return assets;
  const preferred = preferredBackend(shells.os, shells.shells.map((shell) => shell.backend));
  return preferred ? withAgentShell(assets, machine, preferred) : assets;
}

/** The machine an environment key names, or `null` for this machine and anything unrecognized. */
function machineOfEnvKey(key: string): RunTarget | null {
  if (key.startsWith("wsl:") && key.length > 4) return { kind: "wsl", distro: key.slice(4) };
  if (key.startsWith("ssh:") && key.length > 4) return { kind: "ssh", machineId: key.slice(4) };
  return null;
}

/** Every machine's last shell probe, keyed by environment key. */
export function listMachineShells(): Promise<Record<string, MachineShells>> {
  return invoke<Record<string, MachineShells>>("list_machine_shells");
}

/** Probes one machine for its shell backends now; `null` is this machine. */
export function probeMachineShells(machine: RunTarget | null): Promise<MachineShells> {
  return invoke<MachineShells>("probe_machine_shells", { machine });
}
