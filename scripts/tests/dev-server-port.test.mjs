import assert from "node:assert/strict";
import { createServer } from "node:net";
import test from "node:test";

import {
  DEFAULT_BACKEND_PORT,
  DEFAULT_FRONTEND_PORT,
  chooseDevServerPorts,
  chooseLoopbackPort,
  isPortUnavailableError,
  parsePort
} from "../dev-server-port.mjs";

/** Holds a loopback port for the duration of `body`, then releases it. */
async function whileHolding(ports, body) {
  const servers = [];
  try {
    for (const port of ports) {
      const server = createServer((socket) => socket.destroy());
      await new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen({ host: "127.0.0.1", port, exclusive: true }, resolve);
      });
      servers.push(server);
    }
    return await body();
  } finally {
    for (const server of servers) {
      await new Promise((resolve) => server.close(resolve));
    }
  }
}

test("an absent or blank port is no instruction at all", () => {
  assert.equal(parsePort(undefined, "PORT"), null);
  assert.equal(parsePort("", "PORT"), null);
  assert.equal(parsePort("   ", "PORT"), null);
  assert.equal(parsePort(" 1420 ", "PORT"), 1420);
});

test("rejects ports that are not plain numbers in the unprivileged range", () => {
  for (const invalid of ["+1420", "-1420", "0x1420", "14 20", "abc", "1420.0"]) {
    assert.throws(() => parsePort(invalid, "PORT"), /必须是有效端口/u, invalid);
  }
  for (const invalid of ["0", "80", "1023", "65536", "99999"]) {
    assert.throws(() => parsePort(invalid, "PORT"), /1024–65535/u, invalid);
  }
});

test("only an unavailable port is retried; other faults propagate", () => {
  assert.equal(isPortUnavailableError({ code: "EADDRINUSE" }), true);
  assert.equal(isPortUnavailableError({ code: "EACCES" }), true);
  assert.equal(isPortUnavailableError({ code: "EADDRNOTAVAIL" }), false);
  assert.equal(isPortUnavailableError(undefined), false);
});

test("a free preferred port is taken as-is", async () => {
  const free = await chooseLoopbackPort(0);
  const choice = await chooseLoopbackPort(free.port);
  assert.deepEqual(choice, { port: free.port, preferred: free.port, fellBack: false });
});

test("a busy preferred port degrades to an operating-system assignment", async () => {
  const held = await chooseLoopbackPort(0);
  await whileHolding([held.port], async () => {
    const choice = await chooseLoopbackPort(held.port);
    assert.equal(choice.fellBack, true);
    assert.equal(choice.preferred, held.port);
    assert.notEqual(choice.port, held.port);
    assert.ok(choice.port >= 1024 && choice.port <= 65535);
  });
});

test("port 0 is no preference, so its outcome is reported as a fallback", async () => {
  const choice = await chooseLoopbackPort(0);
  assert.equal(choice.fellBack, true);
  assert.ok(choice.port > 0);
});

test("both defaults are taken when both are free", async () => {
  const { frontend, backend } = await chooseDevServerPorts({});
  // Another process on this machine may legitimately hold a default port, so
  // assert the contract rather than the numbers: a default is either honoured
  // or reported as a fallback, never silently replaced.
  assert.equal(frontend.preferred, DEFAULT_FRONTEND_PORT);
  assert.equal(backend.preferred, DEFAULT_BACKEND_PORT);
  assert.equal(frontend.fellBack, frontend.port !== DEFAULT_FRONTEND_PORT);
  assert.equal(backend.fellBack, backend.port !== DEFAULT_BACKEND_PORT);
  assert.notEqual(frontend.port, backend.port);
});

test("explicit ports are an instruction and are never replaced", async () => {
  const held = await chooseLoopbackPort(0);
  const other = await chooseLoopbackPort(0);
  await whileHolding([held.port], async () => {
    const { frontend, backend } = await chooseDevServerPorts({
      explicitFrontend: held.port,
      explicitBackend: other.port
    });
    assert.deepEqual(frontend, { port: held.port, preferred: held.port, fellBack: false });
    assert.equal(backend.port, other.port);
  });
});

test("the two servers never land on the same port", async () => {
  const { frontend, backend } = await chooseDevServerPorts({
    preferredFrontend: DEFAULT_FRONTEND_PORT,
    preferredBackend: DEFAULT_FRONTEND_PORT
  });
  assert.notEqual(frontend.port, backend.port);
  // The backend still reports the preference it could not have, so the operator
  // is told which default moved rather than seeing a bare port number.
  assert.equal(backend.preferred, DEFAULT_FRONTEND_PORT);
  assert.equal(backend.fellBack, true);
});

test("identical explicit ports are a configuration error, not a silent fallback", async () => {
  const held = await chooseLoopbackPort(0);
  await assert.rejects(
    chooseDevServerPorts({ explicitFrontend: held.port, explicitBackend: held.port }),
    /前端与后端端口不能相同/u
  );
});

test("a caller that must contend for fixed ports never moves aside", async () => {
  const heldFrontend = await chooseLoopbackPort(0);
  const heldBackend = await chooseLoopbackPort(0);
  assert.notEqual(heldFrontend.port, heldBackend.port);
  await whileHolding([heldFrontend.port, heldBackend.port], async () => {
    const { frontend, backend } = await chooseDevServerPorts({
      preferredFrontend: heldFrontend.port,
      preferredBackend: heldBackend.port,
      allowFallback: false
    });
    // Busy or not, these are the ports the caller has to be seen contending for.
    assert.deepEqual(frontend, {
      port: heldFrontend.port,
      preferred: heldFrontend.port,
      fellBack: false
    });
    assert.deepEqual(backend, {
      port: heldBackend.port,
      preferred: heldBackend.port,
      fellBack: false
    });
  });
});
