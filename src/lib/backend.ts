import { Channel as TauriChannel, invoke as tauriInvoke } from "@tauri-apps/api/core";

type ChannelHandler<T> = (message: T) => void;

interface PendingInvocation {
  socket: WebSocket;
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}

interface BridgeMessage {
  type?: string;
  id?: number;
  ok?: boolean;
  value?: unknown;
  error?: string;
  channelId?: string;
  payload?: unknown;
}

const browserDevBackendUrl = import.meta.env.VITE_BROWSER_DEV_BACKEND_URL?.trim() ?? "";
const browserDevToken = import.meta.env.VITE_BROWSER_DEV_TOKEN?.trim() ?? "";

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function isBrowserDevRuntime(): boolean {
  return typeof window !== "undefined" && !isTauriRuntime() && Boolean(browserDevBackendUrl);
}

/** True when calls are backed by the real Rust application state. */
export function hasBackendRuntime(): boolean {
  return isTauriRuntime() || isBrowserDevRuntime();
}

function createChannelId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `channel-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

/**
 * Runtime-neutral equivalent of Tauri's Channel. The desktop build wraps a native Tauri channel;
 * browser development registers the same callback on the authenticated local Rust bridge.
 */
export class Channel<T> {
  readonly browserDevId = createChannelId();
  readonly tauriChannel: TauriChannel<T> | null;
  private handler: ChannelHandler<T> | null = null;

  constructor() {
    this.tauriChannel = isTauriRuntime() ? new TauriChannel<T>() : null;
  }

  get onmessage(): ChannelHandler<T> | null {
    return this.handler;
  }

  set onmessage(handler: ChannelHandler<T> | null) {
    this.handler = handler;
    if (this.tauriChannel && handler) this.tauriChannel.onmessage = handler;
  }

  dispatch(payload: unknown): void {
    this.handler?.(payload as T);
  }
}

function containsChannel(value: unknown): boolean {
  if (value instanceof Channel) return true;
  if (Array.isArray(value)) {
    for (const item of value) if (containsChannel(item)) return true;
    return false;
  }
  if (!value || typeof value !== "object") return false;
  for (const key in value) {
    if (Object.hasOwn(value, key)
      && containsChannel((value as Record<string, unknown>)[key])) return true;
  }
  return false;
}

class BrowserDevClient {
  private socket: WebSocket | null = null;
  private connecting: Promise<WebSocket> | null = null;
  private connectingSocket: WebSocket | null = null;
  private hadConnection = false;
  private nextInvocationId = 1;
  private readonly pending = new Map<number, PendingInvocation>();
  private readonly channels = new Map<string, Channel<unknown>>();

  private connect(): Promise<WebSocket> {
    if (this.socket?.readyState === WebSocket.OPEN) return Promise.resolve(this.socket);
    if (this.connecting) return this.connecting;
    if (!browserDevToken) {
      return Promise.reject(new Error("浏览器开发后端缺少会话令牌，请通过 npm run dev:browser 启动"));
    }

    this.connecting = new Promise<WebSocket>((resolve, reject) => {
      const url = new URL(browserDevBackendUrl);
      url.searchParams.set("token", browserDevToken);
      const socket = new WebSocket(url);
      this.connectingSocket = socket;
      let opened = false;

      socket.addEventListener("open", () => {
        opened = true;
        if (this.connectingSocket !== socket) {
          socket.close();
          return;
        }
        const isReconnect = this.hadConnection;
        this.hadConnection = true;
        this.socket = socket;
        this.connecting = null;
        this.connectingSocket = null;
        resolve(socket);
        if (isReconnect) notifyBrowserDevReconnected();
      }, { once: true });
      socket.addEventListener("message", (event) => this.handleMessage(event.data));
      socket.addEventListener("error", () => {
        if (!opened) {
          if (this.connectingSocket === socket) {
            this.connecting = null;
            this.connectingSocket = null;
          }
          reject(new Error("无法连接 Rust 开发后端，请确认 npm run dev:browser 仍在运行"));
        }
      });
      socket.addEventListener("close", () => {
        if (this.socket === socket) this.socket = null;
        // An older CLOSING socket can finish after a replacement connection has already started.
        // Never clear that replacement's promise or reject invocations sent through it.
        if (this.connectingSocket === socket) {
          this.connecting = null;
          this.connectingSocket = null;
        }
        const error = new Error("Rust 开发后端连接已断开");
        for (const [id, invocation] of this.pending) {
          if (invocation.socket !== socket) continue;
          invocation.reject(error);
          this.pending.delete(id);
        }
      });
    });
    return this.connecting;
  }

  private handleMessage(raw: unknown): void {
    if (typeof raw !== "string") return;
    let message: BridgeMessage;
    try {
      message = JSON.parse(raw) as BridgeMessage;
    } catch {
      console.error("Rust 开发后端返回了无效消息", raw);
      return;
    }
    if (message.type === "channel" && message.channelId) {
      this.channels.get(message.channelId)?.dispatch(message.payload);
      return;
    }
    if (message.type !== "result" || typeof message.id !== "number") return;
    const invocation = this.pending.get(message.id);
    if (!invocation) return;
    this.pending.delete(message.id);
    if (message.ok) invocation.resolve(message.value);
    else invocation.reject(new Error(message.error || "Rust 开发后端调用失败"));
  }

  private encode(value: unknown): unknown {
    if (value instanceof Channel) {
      this.channels.set(value.browserDevId, value as Channel<unknown>);
      return { __meworkChannel: value.browserDevId };
    }
    if (Array.isArray(value)) return value.map((item) => this.encode(item));
    if (value && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, this.encode(item)]));
    }
    return value;
  }

  async invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    const socket = await this.connect();
    const id = this.nextInvocationId++;
    const result = new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        socket,
        resolve: resolve as (value: unknown) => void,
        reject: (reason) => reject(reason)
      });
    });
    try {
      socket.send(JSON.stringify({
        id,
        command,
        args: command !== "save_document" && containsChannel(args) ? this.encode(args) : args
      }));
    } catch (error) {
      this.pending.delete(id);
      throw error;
    }
    return result;
  }
}

let browserDevClient: BrowserDevClient | null = null;

type BrowserDevReconnectListener = () => void;

const browserDevReconnectListeners = new Set<BrowserDevReconnectListener>();

/**
 * Browser-dev only: fires after a dropped bridge socket has been replaced by a
 * fresh one (never on the first connection). Per-invoke channels die with
 * their socket, so long-lived backend subscriptions re-register here. The
 * desktop runtime has no reconnects and never fires this.
 */
export function onBrowserDevReconnected(listener: BrowserDevReconnectListener): () => void {
  browserDevReconnectListeners.add(listener);
  return () => browserDevReconnectListeners.delete(listener);
}

function notifyBrowserDevReconnected(): void {
  for (const listener of [...browserDevReconnectListeners]) listener();
}

function encodeTauriArguments(value: unknown): unknown {
  if (value instanceof Channel) return value.tauriChannel;
  if (Array.isArray(value)) return value.map(encodeTauriArguments);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encodeTauriArguments(item)]));
  }
  return value;
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauriRuntime()) {
    return args === undefined
      ? tauriInvoke<T>(command)
      : tauriInvoke<T>(
          command,
          (command !== "save_document" && containsChannel(args) ? encodeTauriArguments(args) : args) as Record<string, unknown>
        );
  }
  if (isBrowserDevRuntime()) {
    browserDevClient ??= new BrowserDevClient();
    return browserDevClient.invoke<T>(command, args ?? {});
  }
  throw new Error("当前页面没有连接 Rust 后端");
}
