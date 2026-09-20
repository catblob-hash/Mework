import { Terminal } from "@xterm/xterm";
import { installCspStyleNonce } from "../lib/cspStyleNonce";

export interface CspXtermProbe {
  terminal: Terminal;
  dispose: () => void;
}

export function openCspXtermProbe(host: HTMLElement): CspXtermProbe {
  const releaseCspStyleNonce = installCspStyleNonce(host.ownerDocument);
  let terminal: Terminal;
  try {
    terminal = new Terminal({ cols: 40, rows: 5 });
    terminal.open(host);
  } catch (error) {
    releaseCspStyleNonce();
    throw error;
  }

  let disposed = false;
  return {
    terminal,
    dispose() {
      if (disposed) return;
      disposed = true;
      try {
        terminal.dispose();
      } finally {
        releaseCspStyleNonce();
      }
    }
  };
}
