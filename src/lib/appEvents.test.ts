import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppPushEvent } from "./appEvents";

const backendMocks = vi.hoisted(() => {
  class FakeChannel<T> {
    onmessage: ((message: T) => void) | null = null;
  }
  return {
    FakeChannel,
    invoke: vi.fn<(command: string, args: Record<string, unknown>) => Promise<unknown>>(),
    hasBackendRuntime: vi.fn(() => true),
    isBrowserDevRuntime: vi.fn(() => true),
    reconnectListeners: [] as Array<() => void>
  };
});

vi.mock("./backend", () => ({
  Channel: backendMocks.FakeChannel,
  invoke: backendMocks.invoke,
  hasBackendRuntime: backendMocks.hasBackendRuntime,
  isBrowserDevRuntime: backendMocks.isBrowserDevRuntime,
  onBrowserDevReconnected: (listener: () => void) => {
    backendMocks.reconnectListeners.push(listener);
    return () => {};
  }
}));

// Subscription state is a module singleton, so reload the module for each test.
async function loadAppEvents() {
  vi.resetModules();
  return await import("./appEvents");
}

function subscribedChannel(callIndex: number) {
  const [command, args] = backendMocks.invoke.mock.calls[callIndex];
  expect(command).toBe("subscribe_app_events");
  return args.onEvent as InstanceType<typeof backendMocks.FakeChannel<AppPushEvent>>;
}

beforeEach(() => {
  backendMocks.invoke.mockReset();
  backendMocks.invoke.mockResolvedValue(undefined);
  backendMocks.hasBackendRuntime.mockReturnValue(true);
  backendMocks.isBrowserDevRuntime.mockReturnValue(true);
  backendMocks.reconnectListeners.length = 0;
});

describe("onAppPushEvent", () => {
  it("首个监听者只建立一次后端订阅，事件广播给所有监听者", async () => {
    const { onAppPushEvent } = await loadAppEvents();
    const first: AppPushEvent[] = [];
    const second: AppPushEvent[] = [];
    onAppPushEvent((event) => first.push(event));
    onAppPushEvent((event) => second.push(event));

    expect(backendMocks.invoke).toHaveBeenCalledTimes(1);
    const channel = subscribedChannel(0);
    channel.onmessage?.({ type: "documentWriteFailure", message: "磁盘已满" });

    expect(first).toEqual([{ type: "documentWriteFailure", message: "磁盘已满" }]);
    expect(second).toEqual(first);
  });

  it("退订后的监听者不再收到事件", async () => {
    const { onAppPushEvent } = await loadAppEvents();
    const received: AppPushEvent[] = [];
    const unsubscribe = onAppPushEvent((event) => received.push(event));
    unsubscribe();

    subscribedChannel(0).onmessage?.({ type: "documentWriteRecovered" });
    expect(received).toEqual([]);
  });

  it("browser-dev 重连后用同一个通道重新订阅", async () => {
    const { onAppPushEvent } = await loadAppEvents();
    onAppPushEvent(() => {});
    expect(backendMocks.reconnectListeners).toHaveLength(1);

    backendMocks.reconnectListeners[0]();

    expect(backendMocks.invoke).toHaveBeenCalledTimes(2);
    expect(subscribedChannel(1)).toBe(subscribedChannel(0));
  });

  it("桌面运行时不注册重连钩子", async () => {
    backendMocks.isBrowserDevRuntime.mockReturnValue(false);
    const { onAppPushEvent } = await loadAppEvents();
    onAppPushEvent(() => {});

    expect(backendMocks.invoke).toHaveBeenCalledTimes(1);
    expect(backendMocks.reconnectListeners).toHaveLength(0);
  });

  it("没有后端运行时完全不订阅", async () => {
    backendMocks.hasBackendRuntime.mockReturnValue(false);
    const { onAppPushEvent } = await loadAppEvents();
    onAppPushEvent(() => {});

    expect(backendMocks.invoke).not.toHaveBeenCalled();
  });
});
