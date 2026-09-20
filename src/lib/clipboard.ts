/**
 * Writes text to the clipboard, falling back to a hidden textarea.
 *
 * The async Clipboard API is refused when the document is not focused, which is
 * routine in an embedded webview, so the legacy path is the one that actually
 * carries the copy in those cases.
 */
export async function writeClipboardText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Fall through to the selection-based path.
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  let copied = false;
  try {
    copied = typeof document.execCommand === "function" && document.execCommand("copy");
  } catch {
    copied = false;
  }
  textarea.remove();
  return copied;
}
