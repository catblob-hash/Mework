import { describe, expect, it } from "vitest";
import type { SshMachineConfig } from "../types";
import {
  browseAncestors,
  browseName,
  browsePath,
  formatAddress,
  joinBrowsePath,
  machineKey,
  parentBrowsePath,
  parseAddress,
  relativeBrowsePath,
  resolveBrowsePath
} from "./fileBrowser";

function sshMachine(id: string, name: string, host: string, port = 0): SshMachineConfig {
  return { id, name, host, port, identityFile: "", createdAt: "", updatedAt: "" };
}

const machines = [
  sshMachine("ubuntu-id", "ubuntu", "dev@100.88.12.34"),
  sshMachine("win-id", "office-pc", "win"),
  sshMachine("lab-id", "lab", "lab.internal", 2222)
];
const posixHost = { sshMachines: machines, hostWindows: false };
const windowsHost = { sshMachines: machines, hostWindows: true };

describe("browse paths", () => {
  it("spells a path the way the host's file service does", () => {
    expect(browsePath("C:\\Projects\\Mework\\")).toBe("C:/Projects/Mework");
    expect(browsePath("\\\\?\\c:\\Projects\\Mework")).toBe("C:/Projects/Mework");
    expect(browsePath("c:")).toBe("C:/");
    expect(browsePath("C:\\")).toBe("C:/");
    expect(browsePath("/home/dev//mework/")).toBe("/home/dev/mework");
    expect(browsePath("/")).toBe("/");
    // A backslash is an ordinary character in a POSIX name.
    expect(browsePath("/srv/a\\b")).toBe("/srv/a\\b");
  });

  it("goes up from a drive to the drives and from a top to the machines", () => {
    expect(parentBrowsePath("/home/dev")).toBe("/home");
    expect(parentBrowsePath("/home")).toBe("/");
    expect(parentBrowsePath("/")).toBeNull();
    expect(parentBrowsePath("C:/Users/dev")).toBe("C:/Users");
    expect(parentBrowsePath("C:/Users")).toBe("C:/");
    expect(parentBrowsePath("C:/")).toBe("/");
  });

  it("names the last segment, a drive by its letter", () => {
    expect(browseName("/home/dev/notes.md")).toBe("notes.md");
    expect(browseName("C:/")).toBe("C:");
    expect(browseName("/")).toBe("/");
    expect(joinBrowsePath("/", "etc")).toBe("/etc");
    expect(joinBrowsePath("C:/", "Users")).toBe("C:/Users");
    expect(joinBrowsePath("/home/dev", "a")).toBe("/home/dev/a");
  });

  it("relates a path to a root, ignoring case only on Windows", () => {
    expect(relativeBrowsePath("/home/dev/mework", "/home/dev/mework/src/App.tsx")).toBe("src/App.tsx");
    expect(relativeBrowsePath("/home/dev/mework", "/home/dev/mework")).toBe("");
    expect(relativeBrowsePath("/home/dev/mework", "/home/dev/mework-old/x")).toBeNull();
    expect(relativeBrowsePath("/home/dev/Mework", "/home/dev/mework/x")).toBeNull();
    expect(relativeBrowsePath("C:/Projects/Mework", "c:/projects/mework/src/x.ts")).toBe("src/x.ts");
    expect(relativeBrowsePath("/", "/etc/hosts")).toBe("etc/hosts");
    expect(relativeBrowsePath("C:/", "C:/Users")).toBe("Users");
    expect(browseAncestors("/w", "/w/a/b/c.ts")).toEqual(["/w/a", "/w/a/b"]);
    expect(browseAncestors("/w", "/elsewhere/c.ts")).toEqual([]);
  });

  it("resolves a written path against the directory it was written in", () => {
    expect(resolveBrowsePath("/home/dev/mework", "src/App.tsx")).toBe("/home/dev/mework/src/App.tsx");
    expect(resolveBrowsePath("/home/dev/mework", "../other/x")).toBe("/home/dev/other/x");
    expect(resolveBrowsePath("/home/dev/mework", "/etc/hosts")).toBe("/etc/hosts");
    expect(resolveBrowsePath("C:/Projects/Mework", "src\\x.ts")).toBe("C:/Projects/Mework/src/x.ts");
    expect(resolveBrowsePath("~/mework", "src/x.ts")).toBe("~/mework/src/x.ts");
    expect(resolveBrowsePath("/", "../../x")).toBe("/x");
  });
});

describe("addresses", () => {
  it("prints this computer's paths bare and other machines' with their identity", () => {
    expect(formatAddress(null, "/home/dev", posixHost)).toBe("/home/dev");
    expect(formatAddress(null, "C:/Users/dev", windowsHost)).toBe("C:\\Users\\dev");
    expect(formatAddress({ kind: "ssh", machineId: "ubuntu-id" }, "/home/dev", posixHost))
      .toBe("dev@100.88.12.34:/home/dev");
    expect(formatAddress({ kind: "ssh", machineId: "lab-id" }, "/srv", posixHost)).toBe("ssh://lab.internal:2222/srv");
    expect(formatAddress({ kind: "wsl", distro: "Ubuntu" }, "/home/dev", windowsHost))
      .toBe("\\\\wsl.localhost\\Ubuntu\\home\\dev");
  });

  it("reads back every address it prints", () => {
    const cases = [
      { machine: null, path: "/home/dev", context: posixHost },
      { machine: { kind: "ssh" as const, machineId: "ubuntu-id" }, path: "/home/dev", context: posixHost },
      { machine: { kind: "ssh" as const, machineId: "lab-id" }, path: "/srv", context: posixHost },
      { machine: { kind: "ssh" as const, machineId: "win-id" }, path: "C:/Users/dev", context: posixHost },
      { machine: { kind: "wsl" as const, distro: "Ubuntu" }, path: "/home/dev", context: windowsHost }
    ];
    for (const { machine, path, context } of cases) {
      const parsed = parseAddress(formatAddress(machine, path, context), context, null);
      expect(parsed, formatAddress(machine, path, context)).toEqual({ kind: "location", machine, path });
    }
    expect(parseAddress("C:\\Users\\dev", windowsHost, null)).toEqual({ kind: "location", machine: null, path: "C:\\Users\\dev" });
  });

  it("knows an SSH machine by its address, its host or its name", () => {
    const ubuntu = { kind: "location", machine: { kind: "ssh", machineId: "ubuntu-id" }, path: "/srv" };
    expect(parseAddress("dev@100.88.12.34:/srv", posixHost, null)).toEqual(ubuntu);
    expect(parseAddress("100.88.12.34:/srv", posixHost, null)).toEqual(ubuntu);
    expect(parseAddress("ubuntu:/srv", posixHost, null)).toEqual(ubuntu);
    expect(parseAddress("ssh://dev@100.88.12.34/srv", posixHost, null)).toEqual(ubuntu);
    expect(parseAddress("ubuntu:", posixHost, null)).toEqual({ ...ubuntu, path: "~" });
    expect(parseAddress("ssh://ubuntu/~/notes", posixHost, null)).toEqual({ ...ubuntu, path: "~/notes" });
    expect(parseAddress("nowhere:/srv", posixHost, null)).toEqual({ kind: "error", reason: "unknown-machine", name: "nowhere" });
    // The port picks between machines on one host.
    expect(parseAddress("ssh://lab.internal:22/srv", posixHost, null)).toEqual({ kind: "error", reason: "unknown-machine", name: "lab.internal" });
  });

  it("takes nothing as the machines, and a relative path against the page", () => {
    expect(parseAddress("  ", posixHost, null)).toEqual({ kind: "machines" });
    const page = { machine: { kind: "ssh" as const, machineId: "ubuntu-id" }, path: "/srv/app" };
    expect(parseAddress("src/../lib", posixHost, page)).toEqual({ kind: "location", machine: page.machine, path: "/srv/app/lib" });
    expect(parseAddress("/etc", posixHost, page)).toEqual({ kind: "location", machine: null, path: "/etc" });
    expect(parseAddress("\\\\wsl$\\Debian\\etc", windowsHost, null)).toEqual({
      kind: "location",
      machine: { kind: "wsl", distro: "Debian" },
      path: "/etc"
    });
  });

  it("keys machines the way the host does", () => {
    expect(machineKey(null)).toBe("local");
    expect(machineKey({ kind: "wsl", distro: "Ubuntu" })).toBe("wsl:Ubuntu");
    expect(machineKey({ kind: "ssh", machineId: "id" })).toBe("ssh:id");
  });
});
