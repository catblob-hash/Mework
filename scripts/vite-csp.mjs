import { randomBytes } from "node:crypto";
import { AsyncLocalStorage } from "node:async_hooks";

export const CSP_NONCE_PLACEHOLDER =
  "__MEWORK_VITE_CSP_NONCE_PLACEHOLDER_6f67c69d8db24df0__";

const LOOPBACK_HOST = "127.0.0.1";
const RUN_ID_PATTERN = /^[0-9a-f]{24}$/;
const TOKEN_PATTERN = /^[0-9a-f]{64}$/;
const requestNonceStorage = new AsyncLocalStorage();
const requestNoncePlaceholder = Object.freeze({
  toString() {
    return requestNonceStorage.getStore()?.nonce ?? CSP_NONCE_PLACEHOLDER;
  },
  [Symbol.toPrimitive]() {
    return requestNonceStorage.getStore()?.nonce ?? CSP_NONCE_PLACEHOLDER;
  }
});

function configuredValue(environment, name) {
  return environment[name]?.trim() ?? "";
}

function requirePattern(environment, name, pattern, description) {
  const value = configuredValue(environment, name);
  if (!pattern.test(value)) throw new Error(`${name} ${description}`);
  return value;
}

function validatedLoopbackUrl(environment, name, { protocol, pathname }) {
  const raw = configuredValue(environment, name);
  if (!raw) throw new Error(`${name} 缺失`);

  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`${name} 必须是有效 URL`);
  }

  if (
    url.protocol !== protocol
    || url.hostname !== LOOPBACK_HOST
    || !url.port
    || url.username
    || url.password
    || url.pathname !== pathname
    || url.search
    || url.hash
  ) {
    throw new Error(
      `${name} 只接受带显式端口的规范 ${protocol}//${LOOPBACK_HOST}${pathname} 回环 URL`
    );
  }
  return url;
}

function anyConfigured(environment, names) {
  return names.some((name) => configuredValue(environment, name));
}

function validatedConnectSources(environment) {
  const sources = [];

  const backendNames = ["VITE_BROWSER_DEV_BACKEND_URL", "VITE_BROWSER_DEV_TOKEN"];
  if (anyConfigured(environment, backendNames)) {
    requirePattern(
      environment,
      "VITE_BROWSER_DEV_TOKEN",
      TOKEN_PATTERN,
      "必须是 32 字节随机小写十六进制令牌"
    );
    sources.push(
      validatedLoopbackUrl(environment, "VITE_BROWSER_DEV_BACKEND_URL", {
        protocol: "ws:",
        pathname: "/ws"
      }).origin
    );
  }

  const memoryNames = [
    "VITE_MEMORY_E2E_ENABLED",
    "VITE_MEMORY_E2E_RUN_ID",
    "VITE_MEMORY_E2E_PROTOCOL_BASE_URL"
  ];
  if (anyConfigured(environment, memoryNames)) {
    if (configuredValue(environment, "VITE_MEMORY_E2E_ENABLED") !== "1") {
      throw new Error("VITE_MEMORY_E2E_ENABLED 只接受精确值 1");
    }
    requirePattern(
      environment,
      "VITE_MEMORY_E2E_RUN_ID",
      RUN_ID_PATTERN,
      "必须是 12 字节随机小写十六进制 run ID"
    );
    validatedLoopbackUrl(environment, "VITE_MEMORY_E2E_PROTOCOL_BASE_URL", {
      protocol: "http:",
      pathname: "/v1"
    });
  }

  const imageNames = [
    "VITE_IMAGE_INPUT_E2E_RUN_ID",
    "VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL",
    "VITE_IMAGE_INPUT_E2E_REPORT_URL",
    "VITE_IMAGE_INPUT_E2E_REPORT_TOKEN"
  ];
  if (anyConfigured(environment, imageNames)) {
    requirePattern(
      environment,
      "VITE_IMAGE_INPUT_E2E_RUN_ID",
      RUN_ID_PATTERN,
      "必须是 12 字节随机小写十六进制 run ID"
    );
    requirePattern(
      environment,
      "VITE_IMAGE_INPUT_E2E_REPORT_TOKEN",
      TOKEN_PATTERN,
      "必须是 32 字节随机小写十六进制令牌"
    );
    validatedLoopbackUrl(environment, "VITE_IMAGE_INPUT_E2E_PROTOCOL_BASE_URL", {
      protocol: "http:",
      pathname: "/v1"
    });
    sources.push(
      validatedLoopbackUrl(environment, "VITE_IMAGE_INPUT_E2E_REPORT_URL", {
        protocol: "http:",
        pathname: "/result"
      }).origin
    );
  }

  const webNames = [
    "VITE_WEB_SEARCH_E2E_RUN_ID",
    "VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL",
    "VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN",
    "VITE_WEB_SEARCH_E2E_REPORT_URL",
    "VITE_WEB_SEARCH_E2E_REPORT_TOKEN"
  ];
  if (anyConfigured(environment, webNames)) {
    requirePattern(
      environment,
      "VITE_WEB_SEARCH_E2E_RUN_ID",
      RUN_ID_PATTERN,
      "必须是 12 字节随机小写十六进制 run ID"
    );
    requirePattern(
      environment,
      "VITE_WEB_SEARCH_E2E_REPORT_TOKEN",
      TOKEN_PATTERN,
      "必须是 32 字节随机小写十六进制令牌"
    );
    validatedLoopbackUrl(environment, "VITE_WEB_SEARCH_E2E_BROWSER_ORIGIN", {
      protocol: "http:",
      pathname: "/"
    });
    validatedLoopbackUrl(environment, "VITE_WEB_SEARCH_E2E_PROTOCOL_BASE_URL", {
      protocol: "http:",
      pathname: "/v1"
    });
    sources.push(
      validatedLoopbackUrl(environment, "VITE_WEB_SEARCH_E2E_REPORT_URL", {
        protocol: "http:",
        pathname: "/result"
      }).origin
    );
  }

  return [...new Set(sources)];
}

function requestFrontendWebSocketSource(request, server) {
  const configuredPort = server.httpServer?.address();
  const listeningPort = typeof configuredPort === "object" && configuredPort
    ? configuredPort.port
    : server.config.server.port;
  const hostHeader = request.headers.host ?? "";
  let host;
  try {
    const parsed = new URL(`http://${hostHeader}`);
    if (
      (parsed.hostname !== LOOPBACK_HOST && parsed.hostname !== "localhost")
      || Number(parsed.port || 80) !== listeningPort
    ) {
      throw new Error("untrusted host");
    }
    host = parsed.host;
  } catch {
    host = `${LOOPBACK_HOST}:${listeningPort}`;
  }
  return `ws://${host}`;
}

export function createContentSecurityPolicy(nonce, connectSources) {
  const connect = [
    "'self'",
    "ipc:",
    "http://ipc.localhost",
    ...connectSources
  ].filter((source, index, sources) => sources.indexOf(source) === index).join(" ");
  return [
    "default-src 'none'",
    `script-src 'nonce-${nonce}' 'self'`,
    "script-src-attr 'none'",
    `style-src 'nonce-${nonce}' 'self'`,
    "style-src-attr 'none'",
    `connect-src ${connect}`,
    "img-src 'self' data:",
    "font-src 'self'",
    "frame-src http: https:",
    "object-src 'none'",
    "worker-src 'none'",
    "child-src 'none'",
    "media-src 'none'",
    "manifest-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'"
  ].join("; ");
}

function installFinalHtmlMiddleware(server, configuredSources) {
  server.middlewares.use((request, response, next) => {
    const pathname = (request.url ?? "").split(/[?#]/, 1)[0];
    const accept = request.headers.accept?.trim() ?? "";
    const acceptsHtml = accept.includes("text/html");
    const mayReturnHtml = request.method === "GET" || request.method === "HEAD";
    if (
      !mayReturnHtml
      || !(pathname === "/" || pathname.endsWith(".html") || acceptsHtml)
    ) {
      next();
      return;
    }

    // A cached body cannot be paired with a newly minted nonce, so HTML is always regenerated.
    delete request.headers["if-none-match"];
    delete request.headers["if-modified-since"];

    const nonce = randomBytes(24).toString("base64");
    const connectSources = [
      requestFrontendWebSocketSource(request, server),
      ...configuredSources
    ];
    response.setHeader("cache-control", "no-store");
    response.setHeader(
      "content-security-policy",
      createContentSecurityPolicy(nonce, [...new Set(connectSources)])
    );

    requestNonceStorage.run({ nonce }, next);
  });
}

export function frontendCspPlugin(environment = process.env) {
  let connectSources = [];
  return {
    name: "mework:frontend-csp",
    config(_config, configEnvironment) {
      if (configEnvironment.command !== "serve") return;
      connectSources = validatedConnectSources(environment);
      // Vite stringifies this unique placeholder only while transforming a request's HTML.
      // AsyncLocalStorage supplies that request's nonce to Vite's own final nonce hook, so
      // React preambles, /@vite/client, page scripts, stylesheets and CSP meta stay aligned
      // without wrapping or buffering Node's response stream.
      return {
        html: { cspNonce: requestNoncePlaceholder },
        server: { headers: { "Cache-Control": "no-store" } }
      };
    },
    configureServer(server) {
      installFinalHtmlMiddleware(server, connectSources);
    }
  };
}
