// Supervision of the `cargo test` child, kept apart from `cargo-test.mjs` so the wrapper's
// lifecycle can be tested with injected adapters instead of a real crate build.
//
// What this is for: `npm run test:rust` used to spawn cargo and do nothing but forward its exit
// code. A Ctrl+C therefore ended the wrapper while cargo and the `mework_lib-<hash>.exe` harness
// it started kept running, and a leftover harness holds its own image open — the next link then
// fails with LNK1104 and waiting does not help, because nothing is going to release it.
//
// Two rules follow from that, and both are pinned by scripts/tests/cargo-test-lifecycle.test.mjs:
//
//   * Ownership is by process id. The wrapper ends the tree rooted at the process *it* started
//     and nothing else. Killing by image name would reach `mework_lib` binaries belonging to
//     another session, another worktree, or the developer's own debugger.
//   * A run that did not finish on its own terms reports a non-zero exit code, and only the
//     child's `close` event counts as finished. Text on the terminal — including a `test result:
//     ok` summary — says nothing about whether the process is still alive.

import { spawn as spawnProcess } from "node:child_process";
import { constants as osConstants } from "node:os";

/** Signals a Ctrl+C, a CI timeout, or a closing console can deliver to the wrapper. */
export const INTERRUPT_SIGNALS = Object.freeze(
  ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"].filter(
    (signal) => Object.hasOwn(osConstants.signals, signal)
  )
);

/** Reported when the run was interrupted but the tree it owned was cleaned up. */
export const INTERRUPTED_EXIT_CODE = 130;

/** Reported when the run ended abnormally: cleanup failed, or cargo could not even start. */
export const ABNORMAL_EXIT_CODE = 1;

/** How long to wait for the child to disappear after the tree has been killed. */
export const CLEANUP_TIMEOUT_MS = 15_000;

function describe(child) {
  return child?.spawnfile ?? "cargo";
}

/** Resolves once the child has closed; rejects if it outlives `timeoutMs`. */
export function waitForChildClose(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.removeListener("close", onClose);
      callback(value);
    };
    const onClose = () => finish(resolve);
    const timer = setTimeout(
      () => finish(reject, new Error(`${describe(child)} 在清理后仍未退出`)),
      timeoutMs
    );
    child.once("close", onClose);
    if (child.exitCode !== null || child.signalCode !== null) finish(resolve);
  });
}

/**
 * Ends the process tree rooted at `pid` — cargo, the test harness it linked, and anything the
 * tests themselves started. `taskkill /T` walks the tree by parent process id, which is exactly
 * why the wrapper must own cargo's own pid rather than a shell wrapped around it.
 */
export function killProcessTree(
  pid,
  { spawn = spawnProcess, platform = process.platform, kill = process.kill.bind(process), timeoutMs = 10_000 } = {}
) {
  if (!Number.isInteger(pid) || pid <= 0) {
    return Promise.reject(new Error(`没有可清理的进程号：${String(pid)}`));
  }
  if (platform !== "win32") {
    try {
      kill(pid, "SIGKILL");
      return Promise.resolve();
    } catch (error) {
      return Promise.reject(error instanceof Error ? error : new Error(String(error)));
    }
  }
  return new Promise((resolve, reject) => {
    // `/PID` and never `/IM`: only this run's tree, never every `mework_lib` on the machine.
    const taskkill = spawn("taskkill.exe", ["/PID", String(pid), "/T", "/F"], {
      windowsHide: true,
      stdio: "ignore"
    });
    let settled = false;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      callback(value);
    };
    const timer = setTimeout(() => {
      taskkill.kill();
      finish(reject, new Error(`taskkill ${pid} 超时`));
    }, timeoutMs);
    taskkill.once("error", (error) => finish(reject, error));
    taskkill.once("close", (code) => {
      // 128 is "no such process": the tree is already gone, which is the outcome we wanted.
      if (code === 0 || code === 128) finish(resolve, undefined);
      else finish(reject, new Error(`taskkill ${pid} 返回 ${String(code)}`));
    });
  });
}

/**
 * Watches `child` until it is really gone and reports what the wrapper should exit with.
 *
 * Resolves to `{ exitCode, message }`; `message` is a line worth printing to stderr, or null.
 * Nothing here calls `process.exit`, so the caller can let Node flush its output first.
 */
export function superviseCargoTest({
  child,
  signalSource = process,
  signals = INTERRUPT_SIGNALS,
  killTree = (pid) => killProcessTree(pid),
  closeTimeoutMs = CLEANUP_TIMEOUT_MS,
  onDiagnostic = () => {}
}) {
  return new Promise((resolve) => {
    let settled = false;
    // Set before any cleanup work starts, so a `close` that arrives synchronously from inside
    // `killTree` cannot be mistaken for the child ending on its own terms.
    let interrupting = false;
    const registered = [];

    const finish = (outcome) => {
      if (settled) return;
      settled = true;
      for (const [signal, handler] of registered) signalSource.off(signal, handler);
      resolve(outcome);
    };

    // `close` and not `exit`: `exit` fires while the child's stdio may still be draining, and a
    // wrapper that reported success there would hand the next build a directory still in use.
    child.once("close", (code, signal) => {
      if (interrupting) return; // The interrupt path owns the outcome.
      if (signal) {
        finish({ exitCode: ABNORMAL_EXIT_CODE, message: `[cargo-test] 被信号 ${signal} 终止` });
        return;
      }
      finish({ exitCode: code ?? ABNORMAL_EXIT_CODE, message: null });
    });

    child.once("error", (error) => {
      if (interrupting) return;
      const reason = error instanceof Error ? error.message : String(error);
      finish({ exitCode: ABNORMAL_EXIT_CODE, message: `[cargo-test] 无法运行 cargo：${reason}` });
    });

    const onSignal = (signal) => {
      if (settled || interrupting) return;
      interrupting = true;
      onDiagnostic(`[cargo-test] 收到 ${signal}，正在结束本次运行启动的进程树…`);
      const cleanup = (async () => {
        await killTree(child.pid);
        await waitForChildClose(child, closeTimeoutMs);
      })();
      cleanup.then(
        () => finish({
          exitCode: INTERRUPTED_EXIT_CODE,
          message: `[cargo-test] 被信号 ${signal} 中断，已清理本次运行的进程树`
        }),
        (error) => finish({
          exitCode: ABNORMAL_EXIT_CODE,
          message: `[cargo-test] 被信号 ${signal} 中断，但清理失败：${
            error instanceof Error ? error.message : String(error)
          }`
        })
      );
    };

    for (const signal of signals) {
      const handler = () => onSignal(signal);
      registered.push([signal, handler]);
      signalSource.on(signal, handler);
    }

    // The child may already be gone by the time we get here, in which case no event is coming.
    if (child.exitCode !== null && child.exitCode !== undefined) {
      finish({ exitCode: child.exitCode, message: null });
    } else if (child.signalCode) {
      finish({
        exitCode: ABNORMAL_EXIT_CODE,
        message: `[cargo-test] 被信号 ${child.signalCode} 终止`
      });
    }
  });
}
