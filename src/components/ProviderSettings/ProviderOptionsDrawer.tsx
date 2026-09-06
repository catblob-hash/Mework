import { useI18n } from "../../i18n";
import { knownFamilySettings, requiredFamilySettings } from "../../lib/modelCapabilities";
import { isClaudeAgentProvider } from "../../lib/claudeAgentProvider";
import { CODEX_DEFAULT_BASE_URL, isBuiltinProvider, isCodexProvider } from "../../lib/codexProvider";
import type { ProviderFamily, ApiProvider } from "../../types";
import { Field } from "../Common";
import { Drawer } from "./Drawer";
import { API_FORMAT_OPTIONS, builtinFamilyOption, endpointLabel, familyLabel, familySettingMeta, NON_CHAT_ENDPOINTS } from "./endpointMeta";

/**
 * Provider settings drawer for the name, chat protocol, and non-chat endpoint URLs.
 *
 * These infrequently changed settings stay out of the main panel so its API key and
 * API URL controls remain prominent. The header gear and API URL controls open this
 * same drawer.
 */
export function ProviderOptionsDrawer({
  provider,
  onChange,
  onClose
}: {
  provider: ApiProvider;
  onChange: (update: (provider: ApiProvider) => ApiProvider) => void;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const settings = knownFamilySettings(provider.family);
  const required = requiredFamilySettings(provider.family);

  return (
    <Drawer
      title={t("提供商设置", "Provider settings")}
      subtitle={isBuiltinProvider(provider)
        ? t("内置提供商", "Built-in provider")
        : t("自定义提供商", "Custom provider")}
      labelledBy="provider-options-title"
      onClose={onClose}
    >
      <Field label={t("提供商名称", "Provider name")}>
        <input
          className="input"
          aria-label={t("提供商名称", "Provider name")}
          value={provider.name}
          onChange={(event) => {
            const name = event.target.value;
            onChange((current) => ({ ...current, name }));
          }}
        />
      </Field>

      <Field
        label={t("API 格式", "API format")}
        hint={t("宿主按它决定对话请求走哪个端点。", "The host uses this to decide which endpoint chat requests go to.")}
      >
        <select
          className="input"
          aria-label={t("API 格式", "API format")}
          value={provider.family}
          disabled={isBuiltinProvider(provider)}
          onChange={(event) => {
            const family = event.target.value as ProviderFamily;
            const oldDefault = API_FORMAT_OPTIONS.find((option) => option.value === provider.family)?.defaultBaseUrl;
            const nextDefault = API_FORMAT_OPTIONS.find((option) => option.value === family)?.defaultBaseUrl ?? "";
            // Preserve an entered URL when changing protocol; replace only an empty URL or the old default.
            const baseUrl = !provider.baseUrl.trim() || provider.baseUrl === oldDefault ? nextDefault : provider.baseUrl;
            // Remove identity fields irrelevant to the new provider family so they cannot
            // be sent with its requests.
            const keep = new Set<string>(knownFamilySettings(family));
            onChange((current) => {
              const familySettings = Object.fromEntries(
                Object.entries(current.familySettings).filter(([key]) => keep.has(key))
              ) as typeof current.familySettings;
              return { ...current, family, baseUrl, familySettings };
            });
          }}
        >
          {builtinFamilyOption(provider.family) ? (
            <option value={provider.family}>{familyLabel(provider.family)}</option>
          ) : API_FORMAT_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>{option.label}</option>
          ))}
        </select>
      </Field>

      {settings.length > 0 && (
        <div className="drawer-section">
          <div className="drawer-section__title">
            <strong>{t("身份字段", "Identity fields")}</strong>
            <small>{t(
              "这一家的地址不是一个固定常量——它由下面这几项推导出来，或者是账号相关的。缺项时宿主会在发请求前指名道姓地拒绝，而不是让上游回一条与凭据无关的错误。",
              "This family's address is not a fixed constant: it is derived from these, or is account-specific. A missing one is refused by name before the request goes out, instead of surfacing as an unrelated upstream error."
            )}</small>
          </div>
          {settings.map((setting) => {
            const meta = familySettingMeta(t, setting);
            const isRequired = required.includes(setting);
            return (
              <Field
                key={setting}
                label={isRequired ? `${meta.label} *` : meta.label}
                hint={meta.hint}
              >
                <input
                  className="input input--code"
                  aria-label={meta.label}
                  spellCheck={false}
                  value={provider.familySettings[setting] ?? ""}
                  placeholder={meta.placeholder}
                  onChange={(event) => {
                    const value = event.target.value;
                    onChange((current) => {
                      const familySettings = { ...current.familySettings };
                      if (value.trim()) familySettings[setting] = value;
                      else delete familySettings[setting];
                      return { ...current, familySettings };
                    });
                  }}
                />
              </Field>
            );
          })}
        </div>
      )}

      {/* Claude Agent has no address at all: the local CLI picks the endpoint and
          the host ignores anything stored here. */}
      {!isClaudeAgentProvider(provider) && (
      <div className="drawer-section">
        <div className="drawer-section__title">
          <strong>{t("端点地址", "Endpoint addresses")}</strong>
          <small>{isCodexProvider(provider) ? t(
            "对话地址属于 ChatGPT 后端，只有测试桩才需要改。图片与语音端点本版本只能配置，宿主还不会向它们发请求。",
            "The chat address belongs to the ChatGPT backend; only a test stub needs to change it. The image and speech endpoints can be configured but the host does not call them in this version."
          ) : t(
            "留空表示沿用上面的 Base URL。图片与语音端点本版本只能配置，宿主还不会向它们发请求。",
            "Leave blank to reuse the Base URL. The image and speech endpoints can be configured but the host does not call them in this version."
          )}</small>
        </div>
        {isCodexProvider(provider) && (
          <Field
            label={t("本机测试桩地址", "Local test stub address")}
            hint={t(
              "留空使用 ChatGPT 后端；只接受本机地址，用于测试。",
              "Blank uses the ChatGPT backend. Only a local address is accepted, for testing."
            )}
          >
            <input
              className="input input--code"
              aria-label={t("本机测试桩地址", "Local test stub address")}
              type="url"
              spellCheck={false}
              value={provider.baseUrl}
              placeholder={CODEX_DEFAULT_BASE_URL}
              onChange={(event) => {
                const baseUrl = event.target.value;
                onChange((current) => ({ ...current, baseUrl }));
              }}
            />
          </Field>
        )}
        {NON_CHAT_ENDPOINTS.map((endpoint) => (
          <Field key={endpoint} label={endpointLabel(t, endpoint)}>
            <input
              className="input input--code"
              aria-label={endpointLabel(t, endpoint)}
              type="url"
              spellCheck={false}
              value={provider.endpointBaseUrls[endpoint] ?? ""}
              placeholder={provider.baseUrl || "https://api.example.com/v1"}
              onChange={(event) => {
                const value = event.target.value;
                onChange((current) => {
                  const endpointBaseUrls = { ...current.endpointBaseUrls };
                  if (value.trim()) endpointBaseUrls[endpoint] = value;
                  else delete endpointBaseUrls[endpoint];
                  return { ...current, endpointBaseUrls };
                });
              }}
            />
          </Field>
        ))}
      </div>
      )}
    </Drawer>
  );
}
