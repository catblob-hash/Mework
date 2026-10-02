import { isTauriRuntime } from "./backend";

/**
 * Who draws the window's frame, which decides where the renderer's own top row sits.
 *
 * The host builds the main window without a system title bar on macOS and Windows
 * (`main_window_chrome` in `lib.rs`), so the two have to agree on the platform:
 *
 * - `mac`: the title bar is transparent and the content runs under it. The traffic lights
 *   stay native and sit over the sidebar's first row, which leaves room for them.
 * - `windows`: there is no frame at all. The content runs to the top edge as on macOS, and
 *   the renderer draws minimize, maximize and close over the top row's right end, where
 *   the chat's top bar leaves room for them.
 * - `none`: the system frame is intact (Linux, and browser-dev in an ordinary tab), so the
 *   renderer draws no window controls and reserves no room for any.
 */
export type WindowChromeKind = "mac" | "windows" | "none";

export function windowChromeKind(): WindowChromeKind {
  if (!isTauriRuntime() || typeof navigator === "undefined") return "none";
  const platform = navigator.platform ?? "";
  if (/^mac/i.test(platform)) return "mac";
  if (/^win/i.test(platform)) return "windows";
  return "none";
}
