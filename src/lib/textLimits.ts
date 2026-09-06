export const MAX_SYSTEM_PROMPT_BYTES = 1024 * 1024;

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}
