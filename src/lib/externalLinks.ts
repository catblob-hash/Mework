import { hasBackendRuntime, invoke } from "./backend";

/**
 * The single exit path for external links in the application.
 *
 * The WebView loads only app documents, so anchors in settings and model-rendered
 * Markdown route through `open_external_url` to the system browser.
 *
 * One document-level capture listener covers runtime Markdown links as well as
 * component links. Capturing calls `preventDefault()` before React so neither
 * the host nor WebView opens the link twice.
 */

/** Matches host `external_open::parse_external_url`: only http(s) URLs are external. */
export function externalHttpUrl(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  let parsed: URL;
  try {
    // A base resolves relative URLs to the app origin, which the origin check below excludes.
    parsed = new URL(trimmed, window.location.href);
  } catch {
    return null;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  // Do not send the app's own origin to the system browser.
  if (parsed.origin === window.location.origin) return null;
  return parsed.toString();
}

/**
 * Opens a URL in the system browser.
 *
 * Browser preview and jsdom fall back to a browser tab because they have no Rust backend.
 */
export async function openExternalUrl(url: string): Promise<void> {
  const target = externalHttpUrl(url);
  if (!target) throw new Error("只能打开 http(s) 外部链接");
  if (!hasBackendRuntime()) {
    window.open(target, "_blank", "noopener,noreferrer");
    return;
  }
  await invoke<void>("open_external_url", { url: target });
}

/**
 * Returns whether this click should be handled here.
 *
 * Modifier clicks and right-clicks keep platform semantics instead of being
 * converted into a normal external-link activation.
 */
function activationClick(event: MouseEvent): boolean {
  if (event.defaultPrevented) return false;
  if (event.button !== 0 && event.button !== 1) return false;
  return !(event.ctrlKey || event.metaKey || event.shiftKey || event.altKey);
}

function anchorTarget(event: Event): string | null {
  const path = typeof event.composedPath === "function" ? event.composedPath() : [];
  const nodes = path.length ? path : [event.target];
  for (const node of nodes) {
    if (!(node instanceof HTMLAnchorElement)) continue;
    // Read the authored attribute, not the browser-resolved `.href`; URL parsing
    // remains centralized in `externalHttpUrl`.
    return node.getAttribute("href");
  }
  return null;
}

/**
 * Installs the document interceptor and returns its cleanup function.
 *
 * `click` covers primary-button and keyboard activation; `auxclick` covers the
 * middle button, which WebView2 otherwise turns into a discarded new-window request.
 */
export function installExternalLinkInterceptor(
  documentRef: Document = document,
  open: (url: string) => Promise<void> = openExternalUrl
): () => void {
  const handle = (event: MouseEvent) => {
    if (!activationClick(event)) return;
    const url = externalHttpUrl(anchorTarget(event));
    if (!url) return;
    event.preventDefault();
    // Suppress the middle-click primary-selection paste on platforms that provide it.
    event.stopPropagation();
    void open(url).catch((error: unknown) => {
      console.error("打开外部链接失败", error);
    });
  };
  documentRef.addEventListener("click", handle, true);
  documentRef.addEventListener("auxclick", handle, true);
  return () => {
    documentRef.removeEventListener("click", handle, true);
    documentRef.removeEventListener("auxclick", handle, true);
  };
}
