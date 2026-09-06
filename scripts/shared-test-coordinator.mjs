import { isProcessAlive } from "./workspace-coordinator.mjs";

export const SHARED_TEST_PROTOCOL = "mework-shared-test-v1";
export const SHARED_TEST_CACHE_TTL_MS = 10 * 60 * 1000;

export function classifySharedTestLease({
  state,
  fingerprint,
  now = Date.now(),
  ownerAlive = isProcessAlive(state?.ownerPid),
  forceRun = false
}) {
  if (!state) return "wait-for-state";
  if (
    state.protocol !== SHARED_TEST_PROTOCOL
    || typeof state.sessionId !== "string"
    || !Number.isSafeInteger(state.ownerPid)
  ) {
    return "incompatible";
  }
  if (state.status === "running") return ownerAlive ? "wait" : "stale";
  if (state.status !== "complete") return "incompatible";
  if (forceRun || state.fingerprint !== fingerprint) return "replace";
  const completedAt = Date.parse(state.completedAt ?? "");
  if (
    !Number.isFinite(completedAt)
    || now - completedAt > SHARED_TEST_CACHE_TTL_MS
  ) {
    return "replace";
  }
  // 只复用明确成功的结果。测试子进程用 stdio: "inherit"，失败诊断没有被保存下来，
  // 复用一条 "exit 1" 摘要会把真实失败细节藏起来；重跑才是唯一能重现诊断的路径。
  const result = state.result;
  if (
    result === null
    || typeof result !== "object"
    || Array.isArray(result)
    || result.exitCode !== 0
    || (result.signal ?? null) !== null
    || result.error !== undefined
  ) {
    return "replace";
  }
  return "reuse";
}
