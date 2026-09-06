import { invoke, isTauriRuntime } from "./backend";

export interface BrowserRendererMutationAuthority {
  rendererMountId: string;
  rendererMountGeneration: number;
}

interface BrowserRendererMountLeaseResponse {
  mountId: string;
  generation: number;
}

interface CachedRendererMount {
  challenge: string;
  lease: BrowserRendererMountLeaseResponse;
  authority: BrowserRendererMutationAuthority;
}

interface ChallengeWaiter {
  resolve: (challenge: string) => void;
  reject: (reason: Error) => void;
}

interface RegistrationAttempt {
  token: symbol;
  challenge: string;
  promise: Promise<BrowserRendererMutationAuthority>;
}

declare global {
  interface Window {
    __MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__?: unknown;
  }
}

const CHALLENGE_EVENT = "mework:browser-renderer-mount-challenge";
const UUID_V4_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const HEARTBEAT_INTERVAL_MS = 4_000;

const RUNTIME_ERROR = "浏览器渲染器写入权限仅可在 Mework 桌面应用中使用";
const CHALLENGE_ERROR = "浏览器渲染器启动验证无效";
const REGISTRATION_ERROR = "无法建立可信的浏览器渲染器写入权限";
const LEASE_ERROR = "浏览器渲染器写入权限响应无效";
const ROTATED_ERROR = "浏览器渲染器写入权限已失效";

let currentChallenge: string | null = null;
let challengeInvalid = false;
let blockedChallenge: string | null = null;
let cachedMount: CachedRendererMount | null = null;
let registrationAttempt: RegistrationAttempt | null = null;
let recoveryChallenge: string | null = null;
let consecutiveRegistrationFailures = 0;
const challengeWaiters = new Set<ChallengeWaiter>();

let heartbeatEnabled = false;
let heartbeatTimer: ReturnType<typeof setTimeout> | null = null;
let heartbeatStartPromise: Promise<void> | null = null;
let heartbeatStartToken: symbol | null = null;

function fixedError(message: string): Error {
  return new Error(message);
}

function isCanonicalUuidV4(value: unknown): value is string {
  return typeof value === "string" && UUID_V4_PATTERN.test(value);
}

function parseLease(value: unknown): BrowserRendererMountLeaseResponse | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  if (keys.length !== 2 || keys[0] !== "generation" || keys[1] !== "mountId") return null;
  if (!isCanonicalUuidV4(record.mountId)) return null;
  if (
    typeof record.generation !== "number"
    || !Number.isSafeInteger(record.generation)
    || record.generation < 1
  ) {
    return null;
  }
  return {
    mountId: record.mountId,
    generation: record.generation
  };
}

function clearHeartbeat(): void {
  heartbeatEnabled = false;
  clearHeartbeatSchedule();
}

function clearHeartbeatSchedule(): void {
  heartbeatStartPromise = null;
  heartbeatStartToken = null;
  if (heartbeatTimer !== null) {
    clearTimeout(heartbeatTimer);
    heartbeatTimer = null;
  }
}

function clearMountForChallengeChange(): void {
  cachedMount = null;
  registrationAttempt = null;
  recoveryChallenge = null;
  consecutiveRegistrationFailures = 0;
  clearHeartbeat();
}

function rejectChallengeWaiters(message: string): void {
  const error = fixedError(message);
  for (const waiter of challengeWaiters) waiter.reject(error);
  challengeWaiters.clear();
}

function resolveChallengeWaiters(challenge: string): void {
  for (const waiter of challengeWaiters) waiter.resolve(challenge);
  challengeWaiters.clear();
}

function acceptChallenge(rawChallenge: unknown): void {
  if (!isCanonicalUuidV4(rawChallenge)) {
    currentChallenge = null;
    challengeInvalid = true;
    blockedChallenge = null;
    clearMountForChallengeChange();
    rejectChallengeWaiters(CHALLENGE_ERROR);
    return;
  }

  if (rawChallenge === currentChallenge) {
    if (blockedChallenge !== rawChallenge) {
      challengeInvalid = false;
      resolveChallengeWaiters(rawChallenge);
    }
    return;
  }

  currentChallenge = rawChallenge;
  challengeInvalid = false;
  blockedChallenge = null;
  clearMountForChallengeChange();
  resolveChallengeWaiters(rawChallenge);
}

function readWindowChallenge(): unknown {
  try {
    return window.__MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__;
  } catch {
    return null;
  }
}

function synchronizeChallengeFromWindow(fromNativeEvent: boolean): void {
  if (!isTauriRuntime() || typeof window === "undefined") return;
  const rawChallenge = readWindowChallenge();
  if (rawChallenge === undefined && !fromNativeEvent && currentChallenge === null) return;
  acceptChallenge(rawChallenge);
}

function handleChallengeEvent(): void {
  synchronizeChallengeFromWindow(true);
}

if (typeof window !== "undefined") {
  window.addEventListener(CHALLENGE_EVENT, handleChallengeEvent);
  synchronizeChallengeFromWindow(false);
}

function waitForUsableChallenge(): Promise<string> {
  synchronizeChallengeFromWindow(false);
  if (challengeInvalid) return Promise.reject(fixedError(CHALLENGE_ERROR));
  if (currentChallenge !== null && blockedChallenge !== currentChallenge) {
    return Promise.resolve(currentChallenge);
  }
  return new Promise<string>((resolve, reject) => {
    challengeWaiters.add({ resolve, reject });
  });
}

function cacheLease(
  challenge: string,
  lease: BrowserRendererMountLeaseResponse
): BrowserRendererMutationAuthority {
  const authority = Object.freeze({
    rendererMountId: lease.mountId,
    rendererMountGeneration: lease.generation
  });
  cachedMount = {
    challenge,
    lease,
    authority
  };
  recoveryChallenge = null;
  consecutiveRegistrationFailures = 0;
  // A transient heartbeat transport failure pauses the cycle but deliberately
  // keeps the renderer's desire to remain mounted. Only an exact native
  // registration retry may restore authority; once it does, resume liveness
  // for that exact lease instead of letting the watchdog revoke it later.
  if (heartbeatEnabled) scheduleHeartbeat(cachedMount);
  return authority;
}

async function registerChallenge(
  challenge: string
): Promise<BrowserRendererMutationAuthority> {
  let rawLease: unknown;
  try {
    rawLease = await invoke<unknown>("browser_register_renderer_mount", { challenge });
  } catch {
    if (currentChallenge !== challenge) throw fixedError(ROTATED_ERROR);

    consecutiveRegistrationFailures += 1;
    if (
      recoveryChallenge === challenge
      || consecutiveRegistrationFailures >= 2
    ) {
      blockedChallenge = challenge;
    }
    throw fixedError(REGISTRATION_ERROR);
  }

  if (currentChallenge !== challenge || blockedChallenge === challenge) {
    throw fixedError(ROTATED_ERROR);
  }

  let lease: BrowserRendererMountLeaseResponse | null;
  try {
    lease = parseLease(rawLease);
  } catch {
    lease = null;
  }
  if (!lease) {
    blockedChallenge = challenge;
    throw fixedError(LEASE_ERROR);
  }
  return cacheLease(challenge, lease);
}

/**
 * Returns the exact native lease every browser-mutating command must attach.
 *
 * The call waits for the native page-load challenge. It deliberately rejects
 * outside Tauri; browser preview must never synthesize renderer authority.
 */
export async function browserRendererMutationAuthority(): Promise<BrowserRendererMutationAuthority> {
  if (!isTauriRuntime()) throw fixedError(RUNTIME_ERROR);

  const challenge = await waitForUsableChallenge();
  if (currentChallenge !== challenge || blockedChallenge === challenge) {
    throw fixedError(ROTATED_ERROR);
  }
  if (
    cachedMount?.challenge === challenge
    && blockedChallenge !== challenge
  ) {
    return cachedMount.authority;
  }
  if (registrationAttempt?.challenge === challenge) {
    return registrationAttempt.promise;
  }

  const token = Symbol("browser-renderer-mount-registration");
  const promise = (async () => {
    try {
      return await registerChallenge(challenge);
    } finally {
      if (registrationAttempt?.token === token) registrationAttempt = null;
    }
  })();
  registrationAttempt = { token, challenge, promise };
  return promise;
}

/**
 * Synchronous, explicitly nullable inspection for non-mutating UI state.
 * Callers that intend to mutate must use `browserRendererMutationAuthority`.
 */
export function browserRendererMutationAuthorityOrNull(): BrowserRendererMutationAuthority | null {
  if (!isTauriRuntime()) return null;
  synchronizeChallengeFromWindow(false);
  if (
    cachedMount === null
    || cachedMount.challenge !== currentChallenge
    || blockedChallenge === currentChallenge
  ) {
    return null;
  }
  return cachedMount.authority;
}

function scheduleHeartbeat(mount: CachedRendererMount): void {
  if (!heartbeatEnabled || cachedMount !== mount || heartbeatTimer !== null) return;
  heartbeatTimer = setTimeout(() => {
    heartbeatTimer = null;
    void sendHeartbeat(mount);
  }, HEARTBEAT_INTERVAL_MS);
}

async function sendHeartbeat(mount: CachedRendererMount): Promise<void> {
  if (!heartbeatEnabled || cachedMount !== mount) return;
  try {
    await invoke<void>("browser_renderer_mount_heartbeat", {
      mountId: mount.lease.mountId,
      generation: mount.lease.generation
    });
  } catch {
    if (cachedMount === mount) {
      cachedMount = null;
      recoveryChallenge = mount.challenge;
      consecutiveRegistrationFailures = 0;
    }
    // Fail closed for mutations immediately, but keep the mounted App's
    // heartbeat intent. A later command can idempotently recover the exact
    // lease before the native watchdog deadline; `cacheLease` will then arm
    // the next heartbeat. We never retry an unknown lease on a timer.
    clearHeartbeatSchedule();
    return;
  }

  if (heartbeatEnabled && cachedMount === mount) scheduleHeartbeat(mount);
}

/**
 * Starts one bounded, non-overlapping heartbeat cycle for the current mount.
 * A rejected heartbeat immediately removes mutation authority and stops.
 */
export function startBrowserRendererMountHeartbeat(): Promise<void> {
  if (!isTauriRuntime()) return Promise.reject(fixedError(RUNTIME_ERROR));
  if (heartbeatEnabled && heartbeatStartPromise) return heartbeatStartPromise;
  if (heartbeatEnabled && cachedMount !== null) return Promise.resolve();

  heartbeatEnabled = true;
  const token = Symbol("browser-renderer-mount-heartbeat-start");
  heartbeatStartToken = token;
  const startPromise = (async () => {
    try {
      await browserRendererMutationAuthority();
      const mount = cachedMount;
      if (heartbeatEnabled && mount !== null) scheduleHeartbeat(mount);
    } catch {
      clearHeartbeat();
      throw fixedError(REGISTRATION_ERROR);
    } finally {
      if (heartbeatStartToken === token) {
        heartbeatStartPromise = null;
        heartbeatStartToken = null;
      }
    }
  })();
  heartbeatStartPromise = startPromise;
  return startPromise;
}

/** Stops heartbeats without sending a renderer-origin teardown mutation. */
export function stopBrowserRendererMountHeartbeat(): void {
  clearHeartbeat();
}
