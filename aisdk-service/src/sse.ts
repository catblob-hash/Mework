//! Line-by-line SSE response-body rewriting infrastructure.
//!
//! Dialect layers wrap `fetch`, split `text/event-stream` bodies into lines,
//! rewrite individual lines, and reassemble them. Centralizing buffering and
//! CRLF handling prevents chunk-boundary omissions.
//!
//! Rules:
//!
//! - Rewrite only bodies; preserve headers and status.
//! - Pass through non-streaming and bodyless responses.
//! - A no-op line rewriter returns the original string reference.
//! - Preserve CRLF delimiters exactly.
//!
/** A `null` return deletes the line and its delimiter. */
export type SseLineRewrite = (line: string) => string | null;

/**
 * Wrap `fetch` to apply a per-response SSE line rewriter from `makeRewrite()`.
 *
 * Stateful rewriters must be created for each response. Request patching applies
 * only to string bodies and returns the original string when no change is needed.
 */
export function sseDialectFetch(
  makeRewrite: () => SseLineRewrite,
  patchRequest?: (body: string) => string,
  inner: typeof globalThis.fetch = globalThis.fetch,
): typeof globalThis.fetch {
  return async (input, init) => {
    let request = init;
    if (patchRequest && init && typeof init.body === "string") {
      const patched = patchRequest(init.body);
      if (patched !== init.body) request = { ...init, body: patched };
    }
    const response = await inner(input, request);
    const contentType = response.headers.get("content-type") ?? "";
    if (!response.body || !contentType.includes("text/event-stream")) {
      return response;
    }

    const rewrite = makeRewrite();
    const decoder = new TextDecoder();
    const encoder = new TextEncoder();
    // A JSON frame may cross chunk boundaries, so buffer through the next newline.
    let pending = "";
    const transform = new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        pending += decoder.decode(chunk, { stream: true });
        let index: number;
        while ((index = pending.indexOf("\n")) >= 0) {
          const raw = pending.slice(0, index);
          pending = pending.slice(index + 1);
          const carriage = raw.endsWith("\r");
          const line = carriage ? raw.slice(0, -1) : raw;
          const rewritten = rewrite(line);
          if (rewritten === null) continue;
          controller.enqueue(encoder.encode(`${rewritten}${carriage ? "\r" : ""}\n`));
        }
      },
      flush(controller) {
        pending += decoder.decode();
        if (pending.length > 0) {
          const rewritten = rewrite(pending);
          if (rewritten !== null) controller.enqueue(encoder.encode(rewritten));
        }
      },
    });

    return new Response(response.body.pipeThrough(transform), {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}
