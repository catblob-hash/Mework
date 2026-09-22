import { Eye, EyeOff, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { ApiKeyStatus } from "../types";
import { IconButton } from "./Common";

export interface SecretFieldCredentials {
  status: () => Promise<ApiKeyStatus>;
  save: (secret: string) => Promise<ApiKeyStatus>;
  reveal: () => Promise<string>;
  remove: () => Promise<ApiKeyStatus>;
}

/**
 * A single credential input shared by fixed provider catalogs.
 *
 * Providers may have multiple secrets. Each field independently handles masking,
 * reveal, blur persistence, and deletion on empty input. The identity is the
 * dependency key for resetting the draft when a provider or slot changes.
 */
export function SecretField({
  label,
  help,
  required,
  onFlush,
  identity,
  credentials,
  onStatusChange
}: {
  label: string;
  help: string;
  required: boolean;
  onFlush?: () => Promise<void>;
  identity: string;
  credentials: SecretFieldCredentials;
  onStatusChange?: (status: ApiKeyStatus) => void;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState<string | null>(null);
  const [keyLength, setKeyLength] = useState(0);
  const [saving, setSaving] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [visible, setVisible] = useState(false);
  const mountedRef = useRef(true);
  const statusTokenRef = useRef(0);
  const saveTokenRef = useRef(0);
  const savedDraftRef = useRef<string | null>(null);
  const visibleRef = useRef(false);
  visibleRef.current = visible;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      statusTokenRef.current += 1;
    };
  }, []);

  // Refresh credential status when the identity changes. Credential identity excludes endpoints.
  // biome-ignore lint/correctness/useExhaustiveDependencies: identity changes define the field lifecycle.
  useEffect(() => {
    const token = statusTokenRef.current + 1;
    statusTokenRef.current = token;
    setDraft(null);
    setVisible(false);
    savedDraftRef.current = null;
    void credentials.status().then((status) => {
      if (!mountedRef.current || statusTokenRef.current !== token) return;
      setKeyLength(status.configured ? status.keyLength ?? 0 : 0);
      onStatusChange?.(status);
    }).catch(() => {
      // A status-read failure is handled by explicit credential operations.
    });
    return () => {
      if (statusTokenRef.current === token) statusTokenRef.current += 1;
    };
  }, [credentials, identity, onStatusChange]);

  const persist = useCallback(async (): Promise<boolean> => {
    const secret = draft?.trim() ?? "";
    if (!secret || savedDraftRef.current === secret) return true;
    const token = saveTokenRef.current + 1;
    saveTokenRef.current = token;
    setSaving(true);
    try {
      await onFlush?.();
      const status = await credentials.save(secret);
      if (!mountedRef.current || saveTokenRef.current !== token) return false;
      setKeyLength(status.keyLength ?? Array.from(secret).length);
      onStatusChange?.(status);
      if (visibleRef.current) savedDraftRef.current = secret;
      else {
        savedDraftRef.current = null;
        setDraft(null);
      }
      return true;
    } catch {
      return false;
    } finally {
      if (mountedRef.current && saveTokenRef.current === token) setSaving(false);
    }
  }, [credentials, draft, onFlush, onStatusChange]);

  const removeStored = useCallback(async () => {
    const token = saveTokenRef.current + 1;
    saveTokenRef.current = token;
    setSaving(true);
    try {
      await onFlush?.();
      const status = await credentials.remove();
      if (!mountedRef.current || saveTokenRef.current !== token) return;
      onStatusChange?.(status);
      savedDraftRef.current = null;
      setDraft(null);
      setKeyLength(0);
    } catch {
      // Preserve the stored credential on deletion failure so blur can retry.
    } finally {
      if (mountedRef.current && saveTokenRef.current === token) setSaving(false);
    }
  }, [credentials, onFlush, onStatusChange]);

  const toggleVisibility = useCallback(async () => {
    if (visibleRef.current) {
      if (!(await persist())) return;
      setVisible(false);
      setDraft(null);
      savedDraftRef.current = null;
      return;
    }
    if (draft !== null) {
      setVisible(true);
      return;
    }
    setRevealing(true);
    try {
      await onFlush?.();
      // Query configuration before revealing the secret; parsing backend copy would
      // couple this path to localized error text.
      const status = await credentials.status();
      if (!mountedRef.current) return;
      onStatusChange?.(status);
      if (!status.configured) {
        setDraft("");
        setVisible(true);
        return;
      }
      const secret = await credentials.reveal();
      if (!mountedRef.current) return;
      setDraft(secret);
      setKeyLength(Array.from(secret).length);
      savedDraftRef.current = secret.trim();
      setVisible(true);
    } catch {
      // Keep the field masked after a read failure so the user can replace it.
    } finally {
      if (mountedRef.current) setRevealing(false);
    }
  }, [credentials, draft, onFlush, onStatusChange, persist]);

  const mask = "•".repeat(keyLength);
  const busyLabel = revealing
    ? t("正在读取{name}", "Reading {name}", { name: label })
    : visible
      ? t("隐藏{name}", "Hide {name}", { name: label })
      : t("显示{name}", "Show {name}", { name: label });

  return (
    <section className="provider-field">
      <div className="provider-field__title">
        <span>{required ? t("{name}（必填）", "{name} (required)", { name: label }) : label}</span>
      </div>
      <div className="provider-field__row">
        <div className="provider-input-group">
          <input
            className="provider-input provider-input--code"
            type={visible ? "text" : "password"}
            aria-label={label}
            autoComplete="new-password"
            value={draft ?? mask}
            onFocus={(event) => {
              if (draft === null && mask) event.currentTarget.select();
            }}
            onClick={(event) => {
              if (draft === null && mask) event.currentTarget.select();
            }}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => {
              if (draft !== null && !draft.trim()) {
                void removeStored();
                return;
              }
              void persist();
            }}
            aria-busy={saving}
            placeholder={t("输入{name}", "Enter {name}", { name: label })}
          />
          <IconButton
            label={busyLabel}
            className="provider-input__reveal"
            disabled={revealing || saving}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => void toggleVisibility()}
          >{revealing
            ? <RefreshCw size={12} className="spin" />
            : visible ? <Eye size={12} /> : <EyeOff size={12} />}</IconButton>
        </div>
      </div>
      <p className="provider-field__help">{help}</p>
    </section>
  );
}
