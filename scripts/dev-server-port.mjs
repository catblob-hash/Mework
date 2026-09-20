// Chooses the loopback ports the development servers listen on.
//
// The framework's default ports are a preference, not a requirement. Another
// checkout, an orphaned run, or an unrelated program may already hold one, and
// refusing to start in that case only ever meant the developer had to go hunting
// for whoever owned the port. Asking the operating system for a free one instead
// always starts, and every consumer of the port learns it from here: the Vite
// command line, the Tauri `devUrl` override, the browser-dev backend address,
// and the host's own record of which origin serves its trusted frontend.

import { createServer } from "node:net";

export const DEFAULT_FRONTEND_PORT = 1420;
export const DEFAULT_BACKEND_PORT = 1430;

const LOOPBACK_HOST = "127.0.0.1";

/**
 * Validates a caller-supplied port, which is an explicit instruction and is
 * therefore never replaced by an operating-system assignment.
 */
export function parsePort(raw, name) {
  if (typeof raw !== "string" || raw.trim() === "") return null;
  const trimmed = raw.trim();
  if (!/^[0-9]{1,5}$/.test(trimmed)) throw new Error(`${name} 必须是有效端口`);
  const port = Number(trimmed);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    throw new Error(`${name} 必须在 1024–65535 范围内`);
  }
  return port;
}

/**
 * Whether a listen error means the port is unavailable to us rather than that
 * something is wrong with the request itself. Only these two are retried on an
 * operating-system assigned port; anything else is a real fault and propagates.
 */
export function isPortUnavailableError(error) {
  return error?.code === "EADDRINUSE" || error?.code === "EACCES";
}

function listenOnce(host, port) {
  return new Promise((resolve, reject) => {
    const server = createServer((socket) => socket.destroy());
    const onError = (error) => {
      server.close(() => reject(error));
    };
    server.once("error", onError);
    // `exclusive` refuses to share the port with another listener in this
    // process's cluster, so a successful bind means the port is genuinely free
    // rather than merely joinable.
    server.listen({ host, port, exclusive: true }, () => {
      server.removeListener("error", onError);
      const assigned = server.address()?.port;
      server.close((closeError) => {
        if (closeError) reject(closeError);
        else if (!Number.isInteger(assigned)) {
          reject(new Error("操作系统没有报告已分配的端口"));
        } else resolve(assigned);
      });
    });
  });
}

/**
 * Returns a loopback port to listen on, preferring `preferred` and falling back
 * to an operating-system assignment when it is taken.
 *
 * The port is released before it is returned, so this reserves nothing: it
 * answers "which port should we ask for", and the caller races anyone else who
 * asks in the same instant. That race is not worth closing here — the caller
 * binds within milliseconds, and a genuine collision surfaces as the listener's
 * own startup failure rather than as a silent wrong answer.
 */
export async function chooseLoopbackPort(preferred, { host = LOOPBACK_HOST } = {}) {
  // Port 0 is "no preference": the operating system picks, and that is already
  // the fallback outcome rather than a preference that was honoured.
  if (preferred !== 0) {
    try {
      return { port: await listenOnce(host, preferred), preferred, fellBack: false };
    } catch (error) {
      if (!isPortUnavailableError(error)) throw error;
    }
  }
  return { port: await listenOnce(host, 0), preferred, fellBack: true };
}

/**
 * Chooses both development-server ports, honouring explicit overrides.
 *
 * `explicitFrontend`/`explicitBackend` come from the environment. An explicit
 * port is an instruction from a harness that has already coordinated its own
 * isolation, so it is used verbatim and never replaced.
 *
 * `allowFallback: false` also pins the preferred ports. Moving aside is only
 * safe when nothing else is expected to be on them: a caller that participates
 * in a rendezvous on fixed ports must be seen contending for those ports, not
 * quietly start somewhere else.
 */
export async function chooseDevServerPorts({
  explicitFrontend = null,
  explicitBackend = null,
  preferredFrontend = DEFAULT_FRONTEND_PORT,
  preferredBackend = DEFAULT_BACKEND_PORT,
  allowFallback = true
} = {}) {
  const pinned = (port) => ({ port, preferred: port, fellBack: false });
  const frontend = explicitFrontend !== null
    ? pinned(explicitFrontend)
    : allowFallback
      ? await chooseLoopbackPort(preferredFrontend)
      : pinned(preferredFrontend);
  let backend;
  if (explicitBackend !== null) {
    backend = pinned(explicitBackend);
  } else if (!allowFallback) {
    backend = pinned(preferredBackend);
  } else {
    // The two servers must not land on the same port. When the frontend had to
    // fall back it may have been handed the backend's preferred port, in which
    // case the backend has no preference left to honour.
    const contested = preferredBackend === frontend.port;
    const chosen = await chooseLoopbackPort(contested ? 0 : preferredBackend);
    backend = { ...chosen, preferred: preferredBackend };
  }
  if (frontend.port === backend.port) {
    throw new Error("浏览器开发前端与后端端口不能相同");
  }
  return { frontend, backend };
}
