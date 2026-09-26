/** Small conversions every previewer needs and none of them owns. */

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/** `m:ss`, or `h:mm:ss` past an hour. */
export function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = String(total % 60).padStart(2, "0");
  return hours ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}

/** The bytes a base64 `data:` URL carries. */
export function dataUrlBytes(source: string): Uint8Array<ArrayBuffer> {
  const comma = source.indexOf(",");
  const binary = atob(comma < 0 ? source : source.slice(comma + 1));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

/** How many bytes a base64 `data:` URL stands for, without decoding it. */
export function dataUrlByteLength(source: string): number {
  const comma = source.indexOf(",");
  const payload = comma < 0 ? source : source.slice(comma + 1);
  const padding = payload.endsWith("==") ? 2 : payload.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((payload.length * 3) / 4) - padding);
}

/** Text as a base64 `data:` URL; `btoa` alone refuses anything outside Latin-1. */
export function textDataUrl(text: string, mediaType: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  }
  return `data:${mediaType};base64,${btoa(binary)}`;
}
