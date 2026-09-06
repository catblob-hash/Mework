import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  ABNORMAL_EXIT_CODE,
  INTERRUPTED_EXIT_CODE,
  INTERRUPT_SIGNALS,
  killProcessTree,
  superviseCargoTest,
  waitForChildClose
} from "../cargo-test-lifecycle.mjs";

// `npm run test:rust` spawns cargo, which links and runs `mework_lib-<hash>.exe`. A wrapper that
// walks away from that tree leaves the harness holding its own image open, and the next link
// fails with LNK1104 forever. These tests drive the wrapper's lifecycle through injected
// adapters, so they can state what happens on Ctrl+C without building the crate.

const scriptsDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const wrapperSource = readFileSync(path.join(scriptsDir, "cargo-test.mjs"), "utf8");

/** A stand-in for the spawned cargo process, with the fields the supervisor reads. */
class FakeChild extends EventEmitter {
  constructor(pid = 4242) {
    super();
    this.pid = pid;
    this.spawnfile = "cargo";
    this.exitCode = null;
    this.signalCode = null;
  }

  /** What cargo does at the end of a green run: prints a summary, exits, then closes. */
  say(line) {
    this.emit("stdout", line);
  }

  exit(code, signal = null) {
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("exit", code, signal);
  }

  close(code, signal = null) {
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("close", code, signal);
  }
}

/** A stand-in for `process`, so a test never installs a real signal handler. */
class FakeSignals extends EventEmitter {
  raise(signal) {
    this.emit(signal);
  }
}

/** Records whether a promise has settled without waiting on it. */
function watch(promise) {
  const state = { settled: false, value: undefined };
  promise.then((value) => {
    state.settled = true;
    state.value = value;
  });
  return state;
}

/** Lets every already-queued microtask and timer callback run. */
function drain() {
  return new Promise((resolve) => setTimeout(resolve, 5));
}

/**
 * Every test here settles within a few ticks. A deadline turns "the supervisor stopped settling"
 * into a failure with a name, instead of a run that hangs until someone notices — which is the
 * very failure mode this file exists to rule out.
 */
function timed(name, body) {
  test(name, { timeout: 5_000 }, body);
}

function supervise(child, overrides = {}) {
  const signalSource = overrides.signalSource ?? new FakeSignals();
  const killed = [];
  const diagnostics = [];
  // A real taskkill makes the child go away; the default stand-in does the same.
  const defaultKill = () => Promise.resolve(child.close(null, "SIGKILL"));
  const outcome = superviseCargoTest({
    child,
    signalSource,
    killTree: (pid) => {
      killed.push(pid);
      return (overrides.killTree ?? defaultKill)(pid);
    },
    closeTimeoutMs: overrides.closeTimeoutMs ?? 200,
    onDiagnostic: (line) => diagnostics.push(line)
  });
  return { outcome, signalSource, killed, diagnostics };
}

timed("a printed success summary is not a finished process", async () => {
  const child = new FakeChild();
  const { outcome } = supervise(child);
  const state = watch(outcome);

  // Everything a green run puts on the terminal, and then the exit event itself.
  child.say("test result: ok. 1560 passed; 0 failed; 0 ignored");
  child.exit(0);
  await drain();
  assert.equal(
    state.settled,
    false,
    "打印成功汇总甚至 exit 事件都不代表进程树已经结束"
  );

  child.close(0);
  await drain();
  assert.equal(state.settled, true);
  assert.equal(state.value.exitCode, 0);
});

timed("the child's own exit code is passed through untouched", async () => {
  for (const code of [0, 1, 101]) {
    const child = new FakeChild();
    const { outcome } = supervise(child);
    child.close(code);
    assert.equal((await outcome).exitCode, code, `退出码 ${code} 应原样传递`);
  }
});

timed("a child killed by a signal is a failure, never a pass", async () => {
  const child = new FakeChild();
  const { outcome } = supervise(child);
  child.close(null, "SIGKILL");
  const result = await outcome;
  assert.notEqual(result.exitCode, 0);
  assert.match(result.message, /SIGKILL/u);
});

timed("cargo failing to start is reported instead of crashing the wrapper", async () => {
  const child = new FakeChild();
  const { outcome } = supervise(child);
  child.emit("error", new Error("spawn cargo ENOENT"));
  const result = await outcome;
  assert.equal(result.exitCode, ABNORMAL_EXIT_CODE);
  assert.match(result.message, /ENOENT/u);
});

timed("an interrupt ends the tree this run started, addressed by process id", async () => {
  const child = new FakeChild(31337);
  const { outcome, signalSource, killed, diagnostics } = supervise(child);
  const state = watch(outcome);

  signalSource.raise("SIGINT");
  await drain();

  assert.deepEqual(killed, [31337], "只能清理本次运行拿到的进程号");
  assert.equal(state.settled, true);
  assert.notEqual(state.value.exitCode, 0, "被中断的运行不能报成功");
  assert.equal(state.value.exitCode, INTERRUPTED_EXIT_CODE);
  assert.ok(diagnostics.some((line) => line.includes("SIGINT")));
});

timed("every interrupt signal a console can deliver is handled", async () => {
  for (const signal of INTERRUPT_SIGNALS) {
    const child = new FakeChild();
    const { outcome, signalSource, killed } = supervise(child);
    signalSource.raise(signal);
    const result = await outcome;
    assert.deepEqual(killed, [child.pid], `${signal} 应触发清理`);
    assert.notEqual(result.exitCode, 0);
  }
});

timed("a second Ctrl+C does not start a second cleanup", async () => {
  const child = new FakeChild();
  let resolveKill;
  const { outcome, signalSource, killed } = supervise(child, {
    killTree: () => new Promise((resolve) => {
      resolveKill = resolve;
    })
  });

  signalSource.raise("SIGINT");
  await drain();
  signalSource.raise("SIGINT");
  signalSource.raise("SIGTERM");
  await drain();
  assert.deepEqual(killed, [child.pid], "清理必须只进行一次");

  resolveKill();
  child.close(null, "SIGKILL");
  assert.notEqual((await outcome).exitCode, 0);
});

timed("a cleanup that fails is reported non-zero, with the reason", async () => {
  const child = new FakeChild();
  const { outcome, signalSource } = supervise(child, {
    killTree: () => Promise.reject(new Error("拒绝访问"))
  });
  signalSource.raise("SIGINT");
  const result = await outcome;
  assert.equal(result.exitCode, ABNORMAL_EXIT_CODE);
  assert.match(result.message, /拒绝访问/u);
});

timed("a child that survives its own cleanup is a failure, not a pass", async () => {
  const child = new FakeChild();
  // The kill reports success but the process never goes away — exactly the reported symptom.
  const { outcome, signalSource } = supervise(child, {
    killTree: () => Promise.resolve(),
    closeTimeoutMs: 50
  });
  signalSource.raise("SIGINT");
  const result = await outcome;
  assert.equal(result.exitCode, ABNORMAL_EXIT_CODE);
  assert.match(result.message, /仍未退出/u);
});

timed("a run with no process id refuses to guess what to clean up", async () => {
  const child = new FakeChild();
  child.pid = undefined;
  const { outcome, signalSource } = supervise(child, {
    killTree: (pid) => killProcessTree(pid, { platform: "win32", spawn: () => {
      throw new Error("不应该走到 taskkill");
    } })
  });
  signalSource.raise("SIGINT");
  const result = await outcome;
  assert.equal(result.exitCode, ABNORMAL_EXIT_CODE);
  assert.match(result.message, /没有可清理的进程号/u);
});

timed("the wrapper stops listening for signals once the run is over", async () => {
  const child = new FakeChild();
  const { outcome, signalSource, killed } = supervise(child);
  for (const signal of INTERRUPT_SIGNALS) {
    assert.equal(signalSource.listenerCount(signal), 1, `${signal} 应有监听`);
  }

  child.close(0);
  await outcome;
  for (const signal of INTERRUPT_SIGNALS) {
    assert.equal(signalSource.listenerCount(signal), 0, `${signal} 的监听应被摘掉`);
  }
  signalSource.raise("SIGINT");
  await drain();
  assert.deepEqual(killed, [], "运行结束后不得再清理任何东西");
});

timed("waitForChildClose returns at once for a child that has already gone", async () => {
  const child = new FakeChild();
  child.exitCode = 0;
  await waitForChildClose(child, 5);
});

timed("killProcessTree names a process id and never an image", async () => {
  const calls = [];
  const spawn = (command, args) => {
    calls.push({ command, args });
    const fake = new EventEmitter();
    fake.kill = () => {};
    queueMicrotask(() => fake.emit("close", 0));
    return fake;
  };
  await killProcessTree(9001, { platform: "win32", spawn });

  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "taskkill.exe");
  assert.deepEqual(calls[0].args, ["/PID", "9001", "/T", "/F"]);
  const flat = [calls[0].command, ...calls[0].args].join(" ").toLowerCase();
  // `/IM mework_lib*` would also reach other worktrees, other sessions, and the debugger.
  assert.ok(!flat.includes("/im"), "不得按映像名批量杀进程");
  assert.ok(!flat.includes("mework_lib"), "不得按映像名批量杀进程");
});

timed("killProcessTree treats an already-dead tree as cleaned up, other failures as failures", async () => {
  const spawnWith = (code) => () => {
    const fake = new EventEmitter();
    fake.kill = () => {};
    queueMicrotask(() => fake.emit("close", code));
    return fake;
  };
  await killProcessTree(9001, { platform: "win32", spawn: spawnWith(128) });
  await assert.rejects(
    () => killProcessTree(9001, { platform: "win32", spawn: spawnWith(1) }),
    /返回 1/u
  );
});

timed("killProcessTree refuses a missing or nonsensical process id", async () => {
  for (const pid of [undefined, null, 0, -1, "4242"]) {
    await assert.rejects(
      () => killProcessTree(pid, { platform: "win32", spawn: () => {
        throw new Error("不应该走到 taskkill");
      } }),
      /没有可清理的进程号/u,
      `pid=${String(pid)}`
    );
  }
});

// The tests above prove the lifecycle is right; only the wrapper is what `npm run test:rust`
// actually executes.
timed("scripts/cargo-test.mjs runs cargo through this lifecycle", () => {
  assert.match(wrapperSource, /from "\.\/cargo-test-lifecycle\.mjs"/u);
  assert.match(wrapperSource, /superviseCargoTest\(/u);
  // Reporting on `exit` is what let the wrapper walk away from a live tree.
  assert.ok(
    !/child\.on\("exit"/u.test(wrapperSource),
    "不能再在 exit 事件上直接判定运行结束"
  );
  // With `shell: true` the recorded pid is a cmd.exe that usually dies first, and `taskkill /T`
  // cannot reach an orphaned cargo through a parent that is already gone.
  assert.ok(
    !/shell:\s*true/u.test(wrapperSource),
    "必须直接持有 cargo 的进程号，中间不能夹一层 shell"
  );
  // Cleanup only ever goes through the lifecycle module, which addresses a process id. The
  // wrapper must not grow a kill of its own — `taskkill /IM mework_lib*` would also reach
  // another worktree's harness and the developer's debugger.
  assert.ok(
    !/taskkill/iu.test(wrapperSource) && !/\/IM\b/iu.test(wrapperSource),
    "包装器不得自己按映像名清理进程"
  );
});
