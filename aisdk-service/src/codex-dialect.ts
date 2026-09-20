//! ChatGPT-subscription Codex Responses dialect adaptation.
//!
//! Codex accepts only streamed Responses requests, rejects `max_output_tokens`,
//! requires `store:false` and encrypted reasoning in `include`, and sends its SSE
//! body without a `content-type` header. A non-streaming AI SDK call is still
//! possible at this layer, so this wrapper requests SSE and converts its terminal
//! event back to the completed JSON response that API expects.

const ENCRYPTED_REASONING_INCLUDE = "reasoning.encrypted_content";

/** JSON object accepted for the Responses request body. */
type JsonObject = Record<string, unknown>;

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Apply the invariant Codex request shape to a string JSON body.
 *
 * Bodies outside the JSON-object protocol are passed through byte-for-byte. The
 * original `stream` value is retained separately because a request that did not
 * ask to stream must be converted back from the mandatory upstream SSE transport
 * to an AI SDK JSON result.
 */
export function coerceCodexRequestBody(text: string): { body: string; wasStreaming: boolean } {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { body: text, wasStreaming: false };
  }
  if (!isObject(value)) return { body: text, wasStreaming: false };

  const wasStreaming = value.stream === true;
  value.store = false;
  value.stream = true;
  delete value.max_output_tokens;
  if (!Array.isArray(value.include)) {
    value.include = [ENCRYPTED_REASONING_INCLUDE];
  } else if (!value.include.includes(ENCRYPTED_REASONING_INCLUDE)) {
    value.include.push(ENCRYPTED_REASONING_INCLUDE);
  }
  return { body: JSON.stringify(value), wasStreaming };
}

function errorResult(event: JsonObject): { status: number; body: string } {
  const response = isObject(event.response) ? event.response : undefined;
  const responseError = response && isObject(response.error) ? response.error : undefined;
  const message =
    (typeof responseError?.message === "string" && responseError.message)
    || (typeof event.message === "string" && event.message)
    || "Codex 后端返回失败事件";
  const type =
    (typeof responseError?.code === "string" && responseError.code)
    || (typeof event.code === "string" && event.code)
    || "codex_stream_failed";
  return { status: 502, body: JSON.stringify({ error: { message, type } }) };
}

/**
 * Turn Codex's completed SSE transport response into the normal Responses JSON
 * response expected by a non-streaming AI SDK call.
 */
export function responseFromCompletedSse(text: string): { status: number; body: string } {
  for (const line of text.split(/\r?\n/)) {
    if (!line.startsWith("data:")) continue;
    const payload = line.slice("data:".length).trim();
    if (payload.length === 0 || payload === "[DONE]") continue;
    let event: unknown;
    try {
      event = JSON.parse(payload);
    } catch {
      continue;
    }
    if (!isObject(event) || typeof event.type !== "string") continue;
    if (event.type === "response.completed" || event.type === "response.incomplete") {
      // Completed Responses events always carry `response`; retain the literal
      // provider object rather than reconstructing a lossy approximation.
      return { status: 200, body: JSON.stringify(event.response) ?? "null" };
    }
    if (event.type === "response.failed" || event.type === "error") return errorResult(event);
  }
  return {
    status: 502,
    body: JSON.stringify({
      error: {
        message: "Codex 后端在没有终止事件的情况下结束了响应",
        type: "codex_stream_failed",
      },
    }),
  };
}

/**
 * Adapt Codex's required SSE transport without changing its authorization model.
 * The host supplies the OAuth bearer token and ChatGPT account headers directly to
 * `createOpenAI`; this layer only normalizes the backend's protocol dialect.
 */
export function codexDialectFetch(
  inner: typeof globalThis.fetch = globalThis.fetch,
): typeof globalThis.fetch {
  return async (input, init) => {
    let request = init;
    let wasStreaming = false;
    if (init && typeof init.body === "string") {
      const coerced = coerceCodexRequestBody(init.body);
      wasStreaming = coerced.wasStreaming;
      request = { ...init, body: coerced.body };
    }

    const response = await inner(input, request);
    if (response.status !== 200) return response;

    if (wasStreaming) {
      if (response.headers.has("content-type")) return response;
      const headers = new Headers(response.headers);
      headers.set("content-type", "text/event-stream");
      return new Response(response.body, {
        status: response.status,
        statusText: response.statusText,
        headers,
      });
    }

    const completed = responseFromCompletedSse(await response.text());
    return new Response(completed.body, {
      status: completed.status,
      headers: { "content-type": "application/json" },
    });
  };
}
