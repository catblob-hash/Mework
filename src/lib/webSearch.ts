import type {
  ApiKeyStatus,
  ConversationWebSearchSettings,
  ProviderFamily,
  SearchCapability,
  SearchProviderKind,
  WebSearchAssets
} from "../types";
import { hasBackendRuntime, invoke } from "./backend";
import { searchProviderSupports } from "./searchProviders";

/**
 * Whether a provider family has a server-side page-fetch tool whose result the
 * host can read as text.
 *
 * Mirrors Rust `web_search::family_supports_native_fetch`. Only Anthropic both
 * exposes fetching as its own server tool and returns the page as plain text;
 * the Responses API has no page-fetch tool at all, and neither do Google or
 * xAI. Search is deliberately not symmetrical with this — no family's *search*
 * results carry readable page text — so this answers only "can this
 * conversation fetch a page without borrowing a provider".
 */
export function familySupportsNativeFetch(family: ProviderFamily | undefined): boolean {
  return family === "anthropic" || family === "bedrock";
}

/**
 * Whether this family spells its native web tools the Messages way, with the
 * version written into the tool's own `type`.
 *
 * Mirrors Rust `web_search::family_selects_native_tool_type`. Every family that
 * has native web tools at all has exactly one shape for them; only Messages
 * makes the version part of the wire, so only there is there anything to pick.
 * A conversation on any other family keeps carrying whichever version it chose
 * and simply does not send it — the selection is never rewritten, so moving
 * back to a Messages model moves back to that same version.
 */
export function familySelectsNativeToolType(family: ProviderFamily | undefined): boolean {
  return family === "anthropic" || family === "bedrock";
}

/**
 * Whether these settings grant `web_fetch` at all.
 *
 * Mirrors the fetch half of Rust `WebSearchSettings::effective`. The renderer
 * needs the same answer for one reason only: the tool lock records which web
 * tools a run actually put in front of the model, and `web_fetch` is the one
 * that may or may not appear. Getting it wrong is visible but not dangerous —
 * a lock that thinks fetching happened greys a selector the user could still
 * have moved; the host remains the only thing that decides what is granted.
 *
 * The search selection is not consulted. Every fetch selection names its own
 * backend, so what searches has no say in what fetches.
 */
export function grantsWebFetch(
  webSearchEnabled: boolean,
  conversation: ConversationWebSearchSettings,
  assets: WebSearchAssets,
  family: ProviderFamily | undefined
): boolean {
  if (!webSearchEnabled) return false;
  switch (conversation.fetchProvider.kind) {
    case "disabled":
      return false;
    case "native":
      return familySupportsNativeFetch(family);
    case "explicit":
      return resolvesBackend(assets, conversation.fetchProvider.providerKind, "fetchUrls");
  }
}

/**
 * Whether these settings grant `web_search` at all.
 *
 * The search leg used to be implied by web access being on, and the host could
 * be trusted to have granted it. It cannot any more: a conversation may name no
 * search backend while still naming a fetch backend, which is a conversation
 * that retrieves pages it is given and never goes looking for one.
 */
export function grantsWebSearch(
  webSearchEnabled: boolean,
  conversation: ConversationWebSearchSettings,
  assets: WebSearchAssets
): boolean {
  if (!webSearchEnabled) return false;
  switch (conversation.provider.kind) {
    case "disabled":
    case "unavailable":
      return false;
    case "native":
      return true;
    case "explicit":
      return resolvesBackend(assets, conversation.provider.providerKind, "searchKeywords");
  }
}

/** A catalog backend resolves when it has the capability and is switched on. */
function resolvesBackend(
  assets: WebSearchAssets,
  kind: SearchProviderKind,
  capability: SearchCapability
): boolean {
  return searchProviderSupports(kind, capability)
    && assets.providers.some((provider) => provider.kind === kind && provider.enabled);
}

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
