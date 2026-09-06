import type { StepError } from "./protocol.js";

/** Request-local credentials only; never rewrite successful model/tool content. */
export function secretsOf(request: {
  apiKey?: string;
  headers?: Record<string, string>;
  agent?: { env?: Record<string, string> };
}): string[] {
  const values = [request.apiKey ?? ""];
  for (const [name, value] of Object.entries(request.headers ?? {})) {
    if (/^(authorization|proxy-authorization|x-api-key|api-key|x-goog-api-key)$/i.test(name)) {
      values.push(value);
      if (/^(?:Bearer|Basic)\s+/i.test(value)) values.push(value.replace(/^\S+\s+/, ""));
    }
  }
  // The claude-agent family carries its only credential — a local test stub's
  // key — inside `agent.env`, and a stub that echoes it in a 401 would otherwise
  // reach the host verbatim.
  for (const name of ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]) {
    const value = request.agent?.env?.[name];
    if (typeof value === "string") values.push(value);
  }
  return [...new Set(values.flatMap((value) => [value, value.trim()]).filter((value) => value.length > 0))]
    .sort((a, b) => b.length - a.length);
}

export function redactSecrets(text: string, secrets: readonly string[]): string {
  // One pass avoids redacting the replacement marker when a short key overlaps it.
  if (secrets.length === 0) return text;
  const escaped = secrets.filter(Boolean).map((secret) => secret.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return escaped.length === 0 ? text : text.replace(new RegExp(escaped.join("|"), "g"), "[redacted]");
}

export function redactError(error: StepError, secrets: readonly string[]): StepError {
  return { ...error, message: redactSecrets(error.message, secrets) };
}
