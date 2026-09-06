import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backendMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  tauri: true
}));

vi.mock("./backend", () => ({
  invoke: backendMocks.invoke,
  isTauriRuntime: () => backendMocks.tauri
}));

type RendererMountModule = typeof import("./browserRendererMount");

const CHALLENGE_EVENT = "mework:browser-renderer-mount-challenge";
const CHALLENGE_ONE = "11111111-1111-4111-8111-111111111111";
const CHALLENGE_TWO = "22222222-2222-4222-8222-222222222222";
const CHALLENGE_THREE = "33333333-3333-4333-8333-333333333333";
const MOUNT_ONE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const MOUNT_TWO = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const THROWING_LEASE = Object.defineProperty({}, "mountId", {
  enumerable: true,
  get: () => {
    throw new Error("native-secret-malformed-lease");
  }
});
Object.defineProperty(THROWING_LEASE, "generation", {
  enumerable: true,
  value: 1
});
const MALFORMED_LEASES: Array<[unknown, string]> = [
  [{ mountId: MOUNT_ONE.toUpperCase(), generation: 1 }, "uppercase id"],
  [{ mountId: MOUNT_ONE, generation: 0 }, "zero generation"],
  [{ mountId: MOUNT_ONE, generation: Number.MAX_SAFE_INTEGER + 1 }, "unsafe generation"],
  [{ mountId: MOUNT_ONE, generation: 1, extra: true }, "extra fields"],
  [{ generation: 1 }, "missing id"]
];

let loadedModule: RendererMountModule | null = null;

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
}

function setChallenge(challenge: unknown): void {
  Object.defineProperty(
    window,
    "__MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__",
    {
      configurable: true,
      writable: true,
      value: challenge
    }
  );
}

function dispatchChallenge(challenge: unknown): void {
  setChallenge(challenge);
  window.dispatchEvent(new Event(CHALLENGE_EVENT));
}

async function loadModule(): Promise<RendererMountModule> {
  loadedModule = await import("./browserRendererMount");
  return loadedModule;
}

describe("browser renderer mount bridge", () => {
  beforeEach(() => {
    backendMocks.invoke.mockReset();
    backendMocks.tauri = true;
    Reflect.deleteProperty(
      window,
      "__MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__"
    );
    vi.resetModules();
    loadedModule = null;
  });

  afterEach(() => {
    loadedModule?.stopBrowserRendererMountHeartbeat();
    vi.clearAllTimers();
    vi.useRealTimers();
    Reflect.deleteProperty(
      window,
      "__MEWORK_BROWSER_RENDERER_MOUNT_CHALLENGE__"
    );
  });

  it("registers a native challenge that arrived before module import", async () => {
    setChallenge(CHALLENGE_ONE);
    backendMocks.invoke.mockResolvedValue({
      mountId: MOUNT_ONE,
      generation: 7
    });
    const bridge = await loadModule();

    await expect(bridge.browserRendererMutationAuthority()).resolves.toEqual({
      rendererMountId: MOUNT_ONE,
      rendererMountGeneration: 7
    });
    expect(backendMocks.invoke).toHaveBeenCalledOnce();
    expect(backendMocks.invoke).toHaveBeenCalledWith(
      "browser_register_renderer_mount",
      { challenge: CHALLENGE_ONE }
    );
  });

  it("waits for a native challenge event that arrives after module import", async () => {
    backendMocks.invoke.mockResolvedValue({
      mountId: MOUNT_ONE,
      generation: 8
    });
    const bridge = await loadModule();
    const authority = bridge.browserRendererMutationAuthority();

    await Promise.resolve();
    expect(backendMocks.invoke).not.toHaveBeenCalled();
    dispatchChallenge(CHALLENGE_ONE);

    await expect(authority).resolves.toEqual({
      rendererMountId: MOUNT_ONE,
      rendererMountGeneration: 8
    });
  });

  it("coalesces concurrent registration into one invoke and one cached authority", async () => {
    setChallenge(CHALLENGE_ONE);
    const registration = deferred<{
      mountId: string;
      generation: number;
    }>();
    backendMocks.invoke.mockReturnValue(registration.promise);
    const bridge = await loadModule();

    const first = bridge.browserRendererMutationAuthority();
    const second = bridge.browserRendererMutationAuthority();
    await Promise.resolve();
    expect(backendMocks.invoke).toHaveBeenCalledOnce();

    registration.resolve({ mountId: MOUNT_ONE, generation: 9 });
    const [firstAuthority, secondAuthority] = await Promise.all([first, second]);
    expect(firstAuthority).toBe(secondAuthority);
    expect(bridge.browserRendererMutationAuthorityOrNull()).toBe(firstAuthority);
    await expect(bridge.browserRendererMutationAuthority()).resolves.toBe(
      firstAuthority
    );
    expect(backendMocks.invoke).toHaveBeenCalledOnce();
  });

  it("drops an in-flight old registration when the native challenge rotates", async () => {
    setChallenge(CHALLENGE_ONE);
    const registrations: Array<{
      resolve: (value: unknown) => void;
    }> = [];
    backendMocks.invoke.mockImplementation(
      () => new Promise((resolve) => registrations.push({ resolve }))
    );
    const bridge = await loadModule();

    const staleAuthority = bridge.browserRendererMutationAuthority();
    await Promise.resolve();
    dispatchChallenge(CHALLENGE_TWO);
    expect(bridge.browserRendererMutationAuthorityOrNull()).toBeNull();
    const currentAuthority = bridge.browserRendererMutationAuthority();
    await Promise.resolve();
    expect(backendMocks.invoke).toHaveBeenCalledTimes(2);

    registrations[0]?.resolve({ mountId: MOUNT_ONE, generation: 10 });
    await expect(staleAuthority).rejects.toThrow("已失效");
    registrations[1]?.resolve({ mountId: MOUNT_TWO, generation: 11 });
    await expect(currentAuthority).resolves.toEqual({
      rendererMountId: MOUNT_TWO,
      rendererMountGeneration: 11
    });
    expect(bridge.browserRendererMutationAuthorityOrNull()).toEqual({
      rendererMountId: MOUNT_TWO,
      rendererMountGeneration: 11
    });
  });

  it("rejects an invalid challenge without invoking native registration", async () => {
    setChallenge("NOT-A-CANONICAL-UUID");
    const bridge = await loadModule();

    await expect(bridge.browserRendererMutationAuthority()).rejects.toThrow(
      "启动验证无效"
    );
    expect(backendMocks.invoke).not.toHaveBeenCalled();

    backendMocks.invoke.mockResolvedValue({
      mountId: MOUNT_ONE,
      generation: 12
    });
    dispatchChallenge(CHALLENGE_ONE);
    await expect(bridge.browserRendererMutationAuthority()).resolves.toEqual({
      rendererMountId: MOUNT_ONE,
      rendererMountGeneration: 12
    });
  });

  it.each(MALFORMED_LEASES)(
    "fails closed for malformed lease response: %s (%s)",
    async (response, _label) => {
      setChallenge(CHALLENGE_ONE);
      backendMocks.invoke.mockResolvedValue(response);
      const bridge = await loadModule();

      await expect(bridge.browserRendererMutationAuthority()).rejects.toThrow(
        "响应无效"
      );
      expect(bridge.browserRendererMutationAuthorityOrNull()).toBeNull();
    }
  );

  it("maps a throwing malformed lease to a fixed response error", async () => {
    setChallenge(CHALLENGE_ONE);
    backendMocks.invoke.mockResolvedValue(THROWING_LEASE);
    const bridge = await loadModule();

    let renderedError = "";
    try {
      await bridge.browserRendererMutationAuthority();
    } catch (error) {
      renderedError = String(error);
    }
    expect(renderedError).toContain("响应无效");
    expect(renderedError).not.toContain("native-secret");
  });

  it("allows an exact same-challenge retry after a lost registration response", async () => {
    setChallenge(CHALLENGE_ONE);
    backendMocks.invoke
      .mockRejectedValueOnce(new Error("native-secret-registration-error"))
      .mockResolvedValueOnce({ mountId: MOUNT_ONE, generation: 13 });
    const bridge = await loadModule();

    let firstError = "";
    try {
      await bridge.browserRendererMutationAuthority();
    } catch (error) {
      firstError = String(error);
    }
    expect(firstError).toContain("无法建立可信");
    expect(firstError).not.toContain("native-secret");

    await expect(bridge.browserRendererMutationAuthority()).resolves.toEqual({
      rendererMountId: MOUNT_ONE,
      rendererMountGeneration: 13
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      1,
      "browser_register_renderer_mount",
      { challenge: CHALLENGE_ONE }
    );
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      2,
      "browser_register_renderer_mount",
      { challenge: CHALLENGE_ONE }
    );
  });

  it("stops and revokes the cached lease when a heartbeat is rejected", async () => {
    vi.useFakeTimers();
    setChallenge(CHALLENGE_ONE);
    backendMocks.invoke
      .mockResolvedValueOnce({ mountId: MOUNT_ONE, generation: 14 })
      .mockRejectedValueOnce(new Error("native-secret-stale-mount"))
      .mockRejectedValueOnce(new Error("native-secret-stale-registration"))
      .mockResolvedValueOnce({ mountId: MOUNT_TWO, generation: 15 });
    const bridge = await loadModule();

    await bridge.startBrowserRendererMountHeartbeat();
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(4_000);

    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      2,
      "browser_renderer_mount_heartbeat",
      { mountId: MOUNT_ONE, generation: 14 }
    );
    expect(bridge.browserRendererMutationAuthorityOrNull()).toBeNull();
    expect(vi.getTimerCount()).toBe(0);

    await expect(bridge.browserRendererMutationAuthority()).rejects.toThrow(
      "无法建立可信"
    );
    const waitingForRotation = bridge.browserRendererMutationAuthority();
    await Promise.resolve();
    expect(backendMocks.invoke).toHaveBeenCalledTimes(3);

    dispatchChallenge(CHALLENGE_TWO);
    await expect(waitingForRotation).resolves.toEqual({
      rendererMountId: MOUNT_TWO,
      rendererMountGeneration: 15
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      4,
      "browser_register_renderer_mount",
      { challenge: CHALLENGE_TWO }
    );
  });

  it("can recover the exact lease idempotently after a transient heartbeat failure", async () => {
    vi.useFakeTimers();
    setChallenge(CHALLENGE_ONE);
    backendMocks.invoke
      .mockResolvedValueOnce({ mountId: MOUNT_ONE, generation: 16 })
      .mockRejectedValueOnce(new Error("temporary-heartbeat-loss"))
      .mockResolvedValueOnce({ mountId: MOUNT_ONE, generation: 16 })
      .mockResolvedValue(undefined);
    const bridge = await loadModule();

    await bridge.startBrowserRendererMountHeartbeat();
    await vi.advanceTimersByTimeAsync(4_000);
    expect(bridge.browserRendererMutationAuthorityOrNull()).toBeNull();
    expect(vi.getTimerCount()).toBe(0);

    await expect(bridge.browserRendererMutationAuthority()).resolves.toEqual({
      rendererMountId: MOUNT_ONE,
      rendererMountGeneration: 16
    });
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      3,
      "browser_register_renderer_mount",
      { challenge: CHALLENGE_ONE }
    );
    expect(vi.getTimerCount()).toBe(1);

    await vi.advanceTimersByTimeAsync(4_000);
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      4,
      "browser_renderer_mount_heartbeat",
      { mountId: MOUNT_ONE, generation: 16 }
    );
    expect(vi.getTimerCount()).toBe(1);

    await vi.advanceTimersByTimeAsync(4_000);
    expect(backendMocks.invoke).toHaveBeenNthCalledWith(
      5,
      "browser_renderer_mount_heartbeat",
      { mountId: MOUNT_ONE, generation: 16 }
    );
    expect(vi.getTimerCount()).toBe(1);
  });

  it("stops an armed heartbeat without a beforeunload mutation", async () => {
    vi.useFakeTimers();
    setChallenge(CHALLENGE_THREE);
    backendMocks.invoke.mockResolvedValue({
      mountId: MOUNT_ONE,
      generation: 17
    });
    const beforeUnloadSpy = vi.spyOn(window, "addEventListener");
    const bridge = await loadModule();

    await bridge.startBrowserRendererMountHeartbeat();
    bridge.stopBrowserRendererMountHeartbeat();
    await vi.advanceTimersByTimeAsync(8_000);

    expect(backendMocks.invoke).toHaveBeenCalledTimes(1);
    expect(beforeUnloadSpy).not.toHaveBeenCalledWith(
      "beforeunload",
      expect.anything()
    );
  });

  it("keeps nullable inspection separate from rejecting non-Tauri authority APIs", async () => {
    backendMocks.tauri = false;
    setChallenge(CHALLENGE_ONE);
    const bridge = await loadModule();

    expect(bridge.browserRendererMutationAuthorityOrNull()).toBeNull();
    await expect(bridge.browserRendererMutationAuthority()).rejects.toThrow(
      "仅可在 Mework 桌面应用"
    );
    await expect(bridge.startBrowserRendererMountHeartbeat()).rejects.toThrow(
      "仅可在 Mework 桌面应用"
    );
    expect(backendMocks.invoke).not.toHaveBeenCalled();
  });
});
