import type { ApiKeyStatus } from "../types";
import { hasBackendRuntime, invoke } from "./backend";

export type DecisionProviderKind = "typesafe";

export const DECISION_PROVIDERS = [
  {
    kind: "typesafe",
    label: "TypeSafe",
    model: "jev-latest",
    docsUrl: "https://docs.typesafe.ai/"
  }
] as const;

export function decisionProviderEntry(kind: DecisionProviderKind) {
  const entry = DECISION_PROVIDERS.find((provider) => provider.kind === kind);
  if (!entry) {
    throw new Error(`Unknown decision provider: ${kind}`);
  }
  return entry;
}

const DECISION_KEY_LENGTH_PREFIX = "mework.decision-api-key-length.v1.";
const DECISION_KEY_PREVIEW_PREFIX = "mework.decision-preview-key-configured.v1.";

let secretMutationTail: Promise<void> = Promise.resolve();

function queueSecretMutation<T>(operation: () => Promise<T>): Promise<T> {
  const result = secretMutationTail.catch(() => undefined).then(operation);
  secretMutationTail = result.then(() => undefined, () => undefined);
  return result;
}

/**
 * The credential identity is the fixed catalog kind, not an endpoint or model
 * field. A catalog row owns one decision-model credential across configuration
 * changes and host environments.
 */
async function credentialFingerprint(kind: DecisionProviderKind): Promise<string> {
  if (!globalThis.crypto?.subtle) throw new Error("当前环境不支持安全的决策模型凭据指纹");
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`mework-decision-model-v1\0${kind}`)
  );
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function rememberKeyLength(kind: DecisionProviderKind, keyLength?: number): Promise<void> {
  const fingerprint = await credentialFingerprint(kind);
  const storageKey = `${DECISION_KEY_LENGTH_PREFIX}${fingerprint}`;
  if (Number.isInteger(keyLength) && (keyLength ?? 0) > 0) {
    window.localStorage.setItem(storageKey, String(keyLength));
  } else {
    window.localStorage.removeItem(storageKey);
  }
}

async function storedKeyLength(kind: DecisionProviderKind): Promise<number | undefined> {
  const fingerprint = await credentialFingerprint(kind);
  const value = Number(window.localStorage.getItem(`${DECISION_KEY_LENGTH_PREFIX}${fingerprint}`));
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

async function browserKeyStatus(kind: DecisionProviderKind): Promise<ApiKeyStatus> {
  const fingerprint = await credentialFingerprint(kind);
  const configured = window.sessionStorage.getItem(`${DECISION_KEY_PREVIEW_PREFIX}${fingerprint}`) === "true";
  return {
    configured,
    keyLength: configured ? await storedKeyLength(kind) : undefined
  };
}

/**
 * Credential commands send only the fixed catalog kind. The Rust side validates
 * it against its own catalog, and no endpoint participates in the identity, so
 * changing host configuration cannot orphan the decision-model secret.
 */
export async function getDecisionKeyStatus(kind: DecisionProviderKind): Promise<ApiKeyStatus> {
  await secretMutationTail;
  if (!hasBackendRuntime()) return browserKeyStatus(kind);
  const status = await invoke<ApiKeyStatus>("get_decision_key_status", { providerKind: kind });
  if (status.configured) {
    const keyLength = status.keyLength ?? await storedKeyLength(kind);
    if (keyLength) await rememberKeyLength(kind, keyLength);
    return { ...status, keyLength };
  }
  await rememberKeyLength(kind);
  return { configured: false };
}

export async function saveDecisionApiKey(
  kind: DecisionProviderKind,
  apiKey: string
): Promise<ApiKeyStatus> {
  return queueSecretMutation(async () => {
    const secret = apiKey.trim();
    if (!secret) throw new Error("凭据不能为空");
    const keyLength = Array.from(secret).length;
    if (hasBackendRuntime()) {
      const status = await invoke<ApiKeyStatus>("save_decision_api_key", {
        providerKind: kind,
        apiKey: secret
      });
      await rememberKeyLength(kind, keyLength);
      return { ...status, keyLength: status.keyLength ?? keyLength };
    }
    const fingerprint = await credentialFingerprint(kind);
    window.sessionStorage.setItem(`${DECISION_KEY_PREVIEW_PREFIX}${fingerprint}`, "true");
    await rememberKeyLength(kind, keyLength);
    return { configured: true, keyLength };
  });
}

export async function revealDecisionApiKey(kind: DecisionProviderKind): Promise<string> {
  await secretMutationTail;
  if (!hasBackendRuntime()) throw new Error("浏览器预览不会保留凭据明文");
  const secret = await invoke<string>("reveal_decision_api_key", { providerKind: kind });
  await rememberKeyLength(kind, Array.from(secret).length);
  return secret;
}

export async function deleteDecisionApiKey(kind: DecisionProviderKind): Promise<ApiKeyStatus> {
  return queueSecretMutation(async () => {
    let status: ApiKeyStatus = { configured: false };
    if (hasBackendRuntime()) {
      status = await invoke<ApiKeyStatus>("delete_decision_api_key", { providerKind: kind });
    } else {
      const fingerprint = await credentialFingerprint(kind);
      window.sessionStorage.removeItem(`${DECISION_KEY_PREVIEW_PREFIX}${fingerprint}`);
    }
    await rememberKeyLength(kind);
    return { ...status, configured: false, keyLength: undefined };
  });
}
