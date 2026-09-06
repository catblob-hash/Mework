import type { ApiKeyStatus } from "../types";
import { hasBackendRuntime, invoke } from "./backend";

/**
 * Secrets a search provider may hold, mirroring Rust's `web_search::CredentialSlot`.
 * `basicAuthPassword` belongs only to SearXNG, which has no API key and may sit
 * behind Basic Auth on self-hosted instances.
 */
export type SearchCredentialSlot = "apiKey" | "basicAuthPassword";

const SEARCH_KEY_LENGTH_PREFIX = "mework.search-api-key-length.v2.";
const SEARCH_KEY_PREVIEW_PREFIX = "mework.search-preview-key-configured.v2.";

let secretMutationTail: Promise<void> = Promise.resolve();

function queueSecretMutation<T>(operation: () => Promise<T>): Promise<T> {
  const result = secretMutationTail.catch(() => undefined).then(operation);
  secretMutationTail = result.then(() => undefined, () => undefined);
  return result;
}

async function credentialFingerprint(providerKind: string, slot: SearchCredentialSlot): Promise<string> {
  if (!globalThis.crypto?.subtle) throw new Error("当前环境不支持安全的搜索凭据指纹");
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`mework-web-search-v2\0${providerKind}\0${slot}`)
  );
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function rememberKeyLength(
  providerKind: string,
  slot: SearchCredentialSlot,
  keyLength?: number
): Promise<void> {
  const fingerprint = await credentialFingerprint(providerKind, slot);
  const storageKey = `${SEARCH_KEY_LENGTH_PREFIX}${fingerprint}`;
  if (Number.isInteger(keyLength) && (keyLength ?? 0) > 0) {
    window.localStorage.setItem(storageKey, String(keyLength));
  } else {
    window.localStorage.removeItem(storageKey);
  }
}

async function storedKeyLength(providerKind: string, slot: SearchCredentialSlot): Promise<number | undefined> {
  const fingerprint = await credentialFingerprint(providerKind, slot);
  const value = Number(window.localStorage.getItem(`${SEARCH_KEY_LENGTH_PREFIX}${fingerprint}`));
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

async function browserKeyStatus(providerKind: string, slot: SearchCredentialSlot): Promise<ApiKeyStatus> {
  const fingerprint = await credentialFingerprint(providerKind, slot);
  const configured = window.sessionStorage.getItem(`${SEARCH_KEY_PREVIEW_PREFIX}${fingerprint}`) === "true";
  return {
    configured,
    keyLength: configured ? await storedKeyLength(providerKind, slot) : undefined
  };
}

/**
 * Credential commands send only the fixed catalog id and a known slot name. The
 * Rust side validates both against its own catalog instead of trusting renderer
 * input — and the endpoint is deliberately **not** part of the identity: a
 * credential is bound to the provider row, so editing an API host never orphans
 * the secret behind it.
 */
export async function getSearchKeyStatus(
  providerKind: string,
  slot: SearchCredentialSlot = "apiKey"
): Promise<ApiKeyStatus> {
  await secretMutationTail;
  if (!hasBackendRuntime()) return browserKeyStatus(providerKind, slot);
  const status = await invoke<ApiKeyStatus>("get_search_key_status", { providerKind, slot });
  if (status.configured) {
    const keyLength = status.keyLength ?? await storedKeyLength(providerKind, slot);
    if (keyLength) await rememberKeyLength(providerKind, slot, keyLength);
    return { ...status, keyLength };
  }
  await rememberKeyLength(providerKind, slot);
  return { configured: false };
}

export async function saveSearchApiKey(
  providerKind: string,
  slot: SearchCredentialSlot,
  apiKey: string
): Promise<ApiKeyStatus> {
  return queueSecretMutation(async () => {
    const secret = apiKey.trim();
    if (!secret) throw new Error("凭据不能为空");
    const keyLength = Array.from(secret).length;
    if (hasBackendRuntime()) {
      const status = await invoke<ApiKeyStatus | boolean | null>("save_search_api_key", {
        providerKind,
        slot,
        apiKey: secret
      });
      if (status === false) throw new Error("凭据未保存");
      await rememberKeyLength(providerKind, slot, keyLength);
      return typeof status === "object" && status && typeof status.configured === "boolean"
        ? { ...status, keyLength: status.keyLength ?? keyLength }
        : { configured: true, keyLength };
    }
    const fingerprint = await credentialFingerprint(providerKind, slot);
    window.sessionStorage.setItem(`${SEARCH_KEY_PREVIEW_PREFIX}${fingerprint}`, "true");
    await rememberKeyLength(providerKind, slot, keyLength);
    return { configured: true, keyLength };
  });
}

export async function revealSearchApiKey(
  providerKind: string,
  slot: SearchCredentialSlot = "apiKey"
): Promise<string> {
  await secretMutationTail;
  if (!hasBackendRuntime()) throw new Error("浏览器预览不会保留凭据明文");
  const secret = await invoke<string>("reveal_search_api_key", { providerKind, slot });
  await rememberKeyLength(providerKind, slot, Array.from(secret).length);
  return secret;
}

export async function deleteSearchApiKey(
  providerKind: string,
  slot: SearchCredentialSlot = "apiKey"
): Promise<ApiKeyStatus> {
  return queueSecretMutation(async () => {
    let status: ApiKeyStatus = { configured: false };
    if (hasBackendRuntime()) {
      status = await invoke<ApiKeyStatus>("delete_search_api_key", { providerKind, slot });
    } else {
      const fingerprint = await credentialFingerprint(providerKind, slot);
      window.sessionStorage.removeItem(`${SEARCH_KEY_PREVIEW_PREFIX}${fingerprint}`);
    }
    await rememberKeyLength(providerKind, slot);
    return { ...status, configured: false, keyLength: undefined };
  });
}
