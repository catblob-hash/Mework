export interface BrowserDevFixtureAddress {
  origin: string;
  port: string;
}

const INVALID_BACKEND_ADDRESS =
  "浏览器 E2E 后端必须是精确的高位 127.0.0.1 WebSocket 地址";

/**
 * Converts the host-issued browser-dev WebSocket address into the matching
 * HTTP fixture origin. It intentionally accepts exactly one address shape:
 * `ws://127.0.0.1:<1024-65535>/ws`.
 */
export function browserDevFixtureAddress(
  rawBackendUrl: string | undefined
): BrowserDevFixtureAddress {
  const raw = rawBackendUrl?.trim();
  if (!raw) throw new Error("浏览器 E2E 缺少隔离后端地址");

  try {
    const backendUrl = new URL(raw);
    const port = Number(backendUrl.port);
    if (
      backendUrl.protocol !== "ws:"
      || backendUrl.hostname !== "127.0.0.1"
      || !backendUrl.port
      || !Number.isInteger(port)
      || port < 1024
      || port > 65_535
      || backendUrl.pathname !== "/ws"
      || backendUrl.username
      || backendUrl.password
      || backendUrl.search
      || backendUrl.hash
      || raw.includes("?")
      || raw.includes("#")
    ) {
      throw new Error(INVALID_BACKEND_ADDRESS);
    }
    const canonicalPort = String(port);
    if (backendUrl.href !== `ws://127.0.0.1:${canonicalPort}/ws`) {
      throw new Error(INVALID_BACKEND_ADDRESS);
    }
    return {
      origin: `http://127.0.0.1:${canonicalPort}`,
      port: canonicalPort
    };
  } catch {
    throw new Error(INVALID_BACKEND_ADDRESS);
  }
}
