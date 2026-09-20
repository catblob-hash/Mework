import { CheckCircle2, CircleAlert, LogIn, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../i18n";
import {
  codexOauthCancelSignIn,
  codexOauthSignIn,
  codexOauthSignOut,
  codexOauthStatus,
} from "../../lib/runtime";
import { CODEX_SIGN_IN_CANCELLED_MESSAGE } from "../../lib/codexProvider";
import type { ApiProvider, CodexOauthStatus } from "../../types";

function planLabel(planType: string | undefined): string {
  if (!planType) return "";
  return `${planType.slice(0, 1).toUpperCase()}${planType.slice(1)}`;
}

export function CodexLoginPanel({
  provider,
  desktopRuntime,
  onSignedInChange,
  onBeforeHostCall,
}: {
  provider: ApiProvider;
  desktopRuntime: boolean;
  onSignedInChange: (signedIn: boolean) => void;
  /**
   * Flushes pending document edits. The host resolves every OAuth command
   * against the persisted provider row, so an unsaved Base URL edit would be
   * rejected as "settings not saved yet" until the debounce fires.
   */
  onBeforeHostCall?: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [status, setStatus] = useState<CodexOauthStatus | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  const mountedRef = useRef(true);
  const requestTokenRef = useRef(0);
  // Settings edits mint a new provider object on every keystroke; only a change
  // of identity should reset the panel, so effects key on the id and read the
  // latest object through this ref.
  const providerRef = useRef(provider);
  providerRef.current = provider;
  const onSignedInChangeRef = useRef(onSignedInChange);
  onSignedInChangeRef.current = onSignedInChange;
  const onBeforeHostCallRef = useRef(onBeforeHostCall);
  onBeforeHostCallRef.current = onBeforeHostCall;
  // One owned poll timer: every action clears it so a cancelled or superseded
  // wait never keeps asking the host in the background.
  const pollRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // The last status the panel showed; a poll that sees "waiting" turn into
  // "signed in" enables the provider exactly like a completed sign-in call,
  // which matters when the renderer reloaded mid-flow and lost that call.
  const lastStatusRef = useRef<CodexOauthStatus | null>(null);

  const stopPolling = () => {
    if (pollRef.current !== undefined) {
      clearTimeout(pollRef.current);
      pollRef.current = undefined;
    }
  };

  const applyStatus = (next: CodexOauthStatus) => {
    const previous = lastStatusRef.current;
    lastStatusRef.current = next;
    setStatus(next);
    if (previous?.signingIn && next.signedIn && !previous.signedIn) {
      onSignedInChangeRef.current(true);
    }
  };

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestTokenRef.current += 1;
      if (pollRef.current !== undefined) clearTimeout(pollRef.current);
    };
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the effect reads the live provider through providerRef; only a change of provider identity (or an explicit retry) restarts it.
  useEffect(() => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    stopPolling();
    lastStatusRef.current = null;
    setStatus(null);
    setPending(false);
    setError(null);

    const current = () => mountedRef.current && requestTokenRef.current === token;
    // Serialized: the next poll is scheduled only after this one answered, so
    // two status reads can never overlap and land out of order.
    const refresh = async () => {
      try {
        await onBeforeHostCallRef.current?.();
        if (!current()) return;
        const next = await codexOauthStatus(providerRef.current);
        if (!current()) return;
        applyStatus(next);
        if (next.signingIn) {
          pollRef.current = setTimeout(() => void refresh(), 2_000);
        }
      } catch (reason) {
        if (current()) setError(reason instanceof Error ? reason.message : String(reason));
      }
    };

    void refresh();
    return () => {
      stopPolling();
    };
  }, [provider.id, reloadNonce]);

  const beginSignIn = async () => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    stopPolling();
    setPending(true);
    setError(null);
    setStatus((current) => current ? { ...current, signingIn: true } : current);
    try {
      await onBeforeHostCallRef.current?.();
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      const next = await codexOauthSignIn(providerRef.current);
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      lastStatusRef.current = next;
      setStatus(next);
      if (next.signedIn) onSignedInChangeRef.current(true);
    } catch (reason) {
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      const message = reason instanceof Error ? reason.message : String(reason);
      if (message !== CODEX_SIGN_IN_CANCELLED_MESSAGE) setError(message);
      setStatus((current) => current ? { ...current, signingIn: false } : current);
    } finally {
      if (mountedRef.current && requestTokenRef.current === token) setPending(false);
    }
  };

  const cancelSignIn = async () => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    stopPolling();
    setPending(false);
    setError(null);
    try {
      await onBeforeHostCallRef.current?.();
      await codexOauthCancelSignIn(providerRef.current);
      const next = await codexOauthStatus(providerRef.current);
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      // A callback that raced the cancel may have completed the sign-in; the
      // host's answer is authoritative either way.
      applyStatus(next);
    } catch (reason) {
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const signOut = async () => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    stopPolling();
    setPending(true);
    setError(null);
    try {
      await onBeforeHostCallRef.current?.();
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      const next = await codexOauthSignOut(providerRef.current);
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      lastStatusRef.current = next;
      setStatus(next);
      onSignedInChangeRef.current(false);
    } catch (reason) {
      if (mountedRef.current && requestTokenRef.current === token) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    } finally {
      if (mountedRef.current && requestTokenRef.current === token) setPending(false);
    }
  };

  if (!status) {
    return <div className="provider-field codex-login">
      <p className="provider-field__help">{error
        ? t("读取登录状态失败。", "Could not read the sign-in status.")
        : t("正在读取登录状态…", "Loading sign-in status…")}</p>
      {error && <p className="provider-field__error" role="alert">{error}</p>}
      {error && <div className="provider-field__row">
        <button
          type="button"
          className="button button--secondary button--small"
          onClick={() => setReloadNonce((nonce) => nonce + 1)}
        >{t("重试", "Retry")}</button>
      </div>}
    </div>;
  }

  if (status.signedIn) {
    const account = status.account;
    return (
      <section className="provider-field codex-login codex-login--signed-in">
        <div className="codex-login__summary">
          <CheckCircle2 size={16} aria-hidden="true" />
          <div>
            <div className="provider-field__title">{t("已登录 ChatGPT", "Signed in to ChatGPT")}</div>
            <p className="provider-field__help">
              {[account?.email, planLabel(account?.planType)].filter(Boolean).join(" · ")}
              {account?.accountId && <> · <code>{account.accountId}</code></>}
            </p>
          </div>
        </div>
        <div className="provider-field__row">
          <button
            type="button"
            className="button button--ghost button--small"
            disabled={pending}
            onClick={() => void signOut()}
          >{pending && <RefreshCw size={12} className="spin" />}{t("退出登录", "Sign out")}</button>
        </div>
        {error && <p className="provider-field__error" role="alert">{error}</p>}
      </section>
    );
  }

  const signingIn = pending || status.signingIn;
  return (
    <section className="provider-field codex-login">
      <div className="codex-login__summary">
        <CircleAlert size={16} aria-hidden="true" />
        <div>
          <div className="provider-field__title">{t("使用 ChatGPT 账号登录", "Sign in with your ChatGPT account")}</div>
          <p className="provider-field__help">{t(
            "使用 ChatGPT Plus / Pro / Team 订阅额度调用 Codex 模型。点击后会打开系统浏览器完成授权，令牌只保存在本机。",
            "Use your ChatGPT Plus / Pro / Team subscription for Codex models. Signing in opens the system browser; tokens stay on this machine."
          )}</p>
        </div>
      </div>
      <div className="provider-field__row">
        <button
          type="button"
          className="button button--primary button--small"
          disabled={!desktopRuntime || signingIn}
          onClick={() => void beginSignIn()}
        >{signingIn ? <RefreshCw size={12} className="spin" /> : <LogIn size={12} />}{signingIn
          ? t("等待浏览器授权…", "Waiting for browser authorization…")
          : t("登录 ChatGPT", "Sign in with ChatGPT")}</button>
        {signingIn && <button
          type="button"
          className="button button--secondary button--small"
          onClick={() => void cancelSignIn()}
        >{t("取消", "Cancel")}</button>}
      </div>
      {!desktopRuntime && <p className="provider-field__help">{t("浏览器预览无法登录 ChatGPT。", "Browser preview cannot sign in to ChatGPT.")}</p>}
      {error && <p className="provider-field__error" role="alert">{error}</p>}
    </section>
  );
}
