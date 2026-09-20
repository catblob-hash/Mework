import { CheckCircle2, CircleAlert, Copy, RefreshCw, TerminalSquare } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { TranslationFunction } from "../../i18n";
import { useI18n } from "../../i18n";
import {
  CLAUDE_AGENT_LEGAL_URL,
  CLAUDE_AGENT_LOGIN_COMMAND,
} from "../../lib/claudeAgentProvider";
import { claudeAgentLoginStatus, claudeAgentOpenLogin } from "../../lib/runtime";
import type { ApiProvider, ClaudeAgentLoginStatus } from "../../types";

function subscriptionLabel(subscriptionType: string | null): string {
  if (!subscriptionType) return "";
  return `${subscriptionType.slice(0, 1).toUpperCase()}${subscriptionType.slice(1)}`;
}

/**
 * Signed-in provenance line: which login the CLI is using. `console` means the
 * CLI is on a Console account and every request is billed per token, which is a
 * materially different deal from a subscription and must not read the same.
 */
function authMethodLabel(t: TranslationFunction, status: ClaudeAgentLoginStatus): string {
  if (status.authMethod === "console") return t("Console（API 计费）", "Console (API billing)");
  return subscriptionLabel(status.subscriptionType);
}

export function ClaudeAgentLoginPanel({
  provider,
  desktopRuntime,
  onSignedInChange,
  onBeforeHostCall,
}: {
  provider: ApiProvider;
  desktopRuntime: boolean;
  onSignedInChange: (signedIn: boolean) => void;
  /**
   * Flushes pending document edits. The host resolves the login commands against
   * the persisted provider row, so an unsaved executable path would be rejected
   * as "settings not saved yet" until the debounce fires.
   */
  onBeforeHostCall?: () => Promise<void>;
}) {
  const { t } = useI18n();
  const [status, setStatus] = useState<ClaudeAgentLoginStatus | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
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
  // The last status the panel showed. A re-check that turns "signed out" into
  // "signed in" enables the provider; an already-signed-in first read is not a
  // transition and must leave a deliberately disabled row alone.
  const lastStatusRef = useRef<ClaudeAgentLoginStatus | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestTokenRef.current += 1;
    };
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the effect reads the live provider through providerRef; only a change of provider identity (or an explicit re-check) restarts it.
  useEffect(() => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    setStatus(null);
    setPending(false);
    setError(null);
    setCopied(false);
    // A preview has no CLI to ask; asking would only produce a bridge error.
    if (!desktopRuntime) return;

    const current = () => mountedRef.current && requestTokenRef.current === token;
    void (async () => {
      try {
        await onBeforeHostCallRef.current?.();
        if (!current()) return;
        const next = await claudeAgentLoginStatus(providerRef.current);
        if (!current()) return;
        const previous = lastStatusRef.current;
        lastStatusRef.current = next;
        setStatus(next);
        if (previous && !previous.signedIn && next.signedIn) {
          onSignedInChangeRef.current(true);
        }
      } catch (reason) {
        if (current()) setError(reason instanceof Error ? reason.message : String(reason));
      }
    })();
  }, [provider.id, desktopRuntime, reloadNonce]);

  const openLogin = async () => {
    const token = requestTokenRef.current + 1;
    requestTokenRef.current = token;
    setPending(true);
    setError(null);
    try {
      await onBeforeHostCallRef.current?.();
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      await claudeAgentOpenLogin(providerRef.current);
    } catch (reason) {
      if (!mountedRef.current || requestTokenRef.current !== token) return;
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (mountedRef.current && requestTokenRef.current === token) setPending(false);
    }
  };

  const copyCommand = async () => {
    setError(null);
    setCopied(false);
    try {
      await navigator.clipboard.writeText(CLAUDE_AGENT_LOGIN_COMMAND);
      if (mountedRef.current) setCopied(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      setError(t("复制命令失败：{reason}", "Could not copy the command: {reason}", {
        reason: reason instanceof Error ? reason.message : String(reason),
      }));
    }
  };

  const compliance = (
    <p className="provider-field__help">
      {t(
        "Mework 复用你本机 Claude Code 的登录状态，仅供本人使用。",
        "Mework reuses the Claude Code login on this machine, for your own use only."
      )}{" "}
      <a href={CLAUDE_AGENT_LEGAL_URL} target="_blank" rel="noreferrer">
        {t("Claude Code 使用条款", "Claude Code terms")}
      </a>
    </p>
  );

  const recheck = (
    <button
      type="button"
      className="button button--secondary button--small"
      onClick={() => setReloadNonce((nonce) => nonce + 1)}
    ><RefreshCw size={12} />{t("重新检查", "Re-check")}</button>
  );

  if (!desktopRuntime) {
    return (
      <section className="provider-field provider-login">
        <div className="provider-login__summary">
          <CircleAlert size={16} aria-hidden="true" />
          <div>
            <div className="provider-field__title">{t("使用本机 Claude Code 的登录", "Uses the Claude Code login on this machine")}</div>
            <p className="provider-field__help">{t(
              "浏览器预览无法读取 Claude Code 登录状态。",
              "Browser preview cannot read the Claude Code sign-in status."
            )}</p>
          </div>
        </div>
        {compliance}
      </section>
    );
  }

  if (!status) {
    return (
      <section className="provider-field provider-login">
        <p className="provider-field__help">{error
          ? t("读取 Claude Code 登录状态失败。", "Could not read the Claude Code sign-in status.")
          : t("正在读取登录状态…", "Loading sign-in status…")}</p>
        {error && <p className="provider-field__error" role="alert">{error}</p>}
        {error && <>
          <p className="provider-field__help">{t(
            "可以在「提供商设置」里填 Claude Code 路径。",
            "You can set the Claude Code executable path under “Provider settings”."
          )}</p>
          <div className="provider-field__row">
            <button
              type="button"
              className="button button--secondary button--small"
              onClick={() => setReloadNonce((nonce) => nonce + 1)}
            >{t("重试", "Retry")}</button>
          </div>
        </>}
        {compliance}
      </section>
    );
  }

  if (status.signedIn) {
    return (
      <section className="provider-field provider-login provider-login--signed-in">
        <div className="provider-login__summary">
          <CheckCircle2 size={16} aria-hidden="true" />
          <div>
            <div className="provider-field__title">{t("已登录 Claude Code", "Signed in to Claude Code")}</div>
            <p className="provider-field__help">
              {[status.email, status.orgName, authMethodLabel(t, status)].filter(Boolean).join(" · ")}
            </p>
          </div>
        </div>
        <div className="provider-field__row">
          {recheck}
        </div>
        {error && <p className="provider-field__error" role="alert">{error}</p>}
        {compliance}
      </section>
    );
  }

  return (
    <section className="provider-field provider-login">
      <div className="provider-login__summary">
        <CircleAlert size={16} aria-hidden="true" />
        <div>
          <div className="provider-field__title">{t("使用本机 Claude Code 的登录", "Uses the Claude Code login on this machine")}</div>
          <p className="provider-field__help">{t(
            "在终端里运行 claude auth login 完成登录（claude.ai 订阅或 Console 账号都可以）。Mework 不读、不搬、也不转发任何凭据。",
            "Run claude auth login in a terminal to sign in (a claude.ai subscription or a Console account both work). Mework never reads, copies, or forwards any credential."
          )}</p>
        </div>
      </div>
      <div className="provider-field__row">
        <button
          type="button"
          className="button button--primary button--small"
          disabled={pending}
          onClick={() => void openLogin()}
        >{pending ? <RefreshCw size={12} className="spin" /> : <TerminalSquare size={12} />}{t("打开终端登录", "Open a terminal to sign in")}</button>
        <button
          type="button"
          className="button button--secondary button--small"
          onClick={() => void copyCommand()}
        ><Copy size={12} />{copied
          ? t("已复制", "Copied")
          : t("复制命令", "Copy command")}</button>
        {recheck}
      </div>
      <p className="provider-field__help provider-field__help--code">{CLAUDE_AGENT_LOGIN_COMMAND}</p>
      {error && <p className="provider-field__error" role="alert">{error}</p>}
      {compliance}
    </section>
  );
}
