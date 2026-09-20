import { Minus, Monitor, Moon, Plus, RotateCcw, Sun } from "lucide-react";
import type { ChangeEvent, JSX, PointerEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { resolveApplicationLanguage, useI18n } from "../../i18n";
import {
  clampMessageFontSize,
  clampZoom,
  COMPOSER_SHORTCUT_CHOICES,
  MAX_MESSAGE_FONT_SIZE,
  MAX_ZOOM,
  MIN_MESSAGE_FONT_SIZE,
  MIN_ZOOM,
  MONO_FONT_PRESETS,
  normalizeHexColor,
  THEME_COLOR_PRESETS,
  UI_FONT_PRESETS,
  ZOOM_STEP
} from "../../lib/appearance";
import { bindingsConflict, shortcutKeyCaps } from "../../lib/shortcuts";
import type {
  AppearancePreferences,
  AppLanguage,
  GlobalSettings,
  KeyToken,
  ThemePreference
} from "../../types";
import { IconButton, Switch } from "../Common";
import { SettingsPageHeading } from "../SettingsPageHeading";
import "./AppearanceSettings.css";

type AppearanceSettingsProps = {
  settings: GlobalSettings;
  onChange: (
    change: GlobalSettings | ((current: GlobalSettings) => GlobalSettings)
  ) => void;
};

type AppearanceUpdater = (
  current: AppearancePreferences
) => AppearancePreferences;

function SettingsCard({
  title,
  children
}: {
  title: string;
  children: React.ReactNode;
}): JSX.Element {
  return (
    <section className="settings-card">
      <div className="settings-card__heading">
        <div>
          <span>
            <strong>{title}</strong>
          </span>
        </div>
      </div>
      <div className="appearance-settings-page__card-body">{children}</div>
    </section>
  );
}

function SettingRow({
  title,
  description,
  children,
  vertical = false
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
  vertical?: boolean;
}): JSX.Element {
  return (
    <div
      className={`appearance-settings-page__row${
        vertical ? " appearance-settings-page__row--vertical" : ""
      }`}
    >
      <div className="appearance-settings-page__row-copy">
        <strong>{title}</strong>
        {description && <small>{description}</small>}
      </div>
      <div className="appearance-settings-page__row-control">{children}</div>
    </div>
  );
}

function FontCombobox({
  label,
  value,
  presets,
  onChange
}: {
  label: string;
  value: string;
  presets: readonly string[];
  onChange: (value: string) => void;
}): JSX.Element {
  const { t } = useI18n();
  const presetSignature = presets.join("|");
  const [customMode, setCustomMode] = useState(() => !presets.includes(value));
  const [customDraft, setCustomDraft] = useState(() =>
    presets.includes(value) ? "" : value
  );

  // Choosing Custom only opens the input; do not persist the default font until it changes.
  // Depend on the font value and a stable string signature to avoid resetting selection every render.
  useEffect(() => {
    const knownPreset = presetSignature.split("|").includes(value);
    setCustomMode(!knownPreset);
    setCustomDraft(knownPreset ? "" : value);
  }, [presetSignature, value]);

  const customValue = "__custom_font__";
  return (
    <div className="appearance-settings-page__font-combobox">
      <select
        className="input"
        aria-label={label}
        value={customMode ? customValue : value}
        onChange={(event) => {
          if (event.target.value === customValue) {
            setCustomMode(true);
            setCustomDraft("");
            return;
          }
          setCustomMode(false);
          setCustomDraft("");
          onChange(event.target.value);
        }}
      >
        {presets.map((family) => (
          <option
            key={family || "default"}
            value={family}
            style={family ? { fontFamily: family } : undefined}
          >
            {family ? t(family, family) : t("默认", "Default")}
          </option>
        ))}
        <option value={customValue}>{t("自定义…", "Custom…")}</option>
      </select>
      {customMode && (
        <input
          className="input"
          aria-label={t("自定义{name}", "Custom {name}", { name: label })}
          placeholder={t("输入字体族", "Enter a font family")}
          value={customDraft}
          onChange={(event) => {
            setCustomDraft(event.target.value);
            onChange(event.target.value);
          }}
        />
      )}
    </div>
  );
}

function shortcutValue(binding: readonly KeyToken[]): string {
  return binding.join("+");
}

function ShortcutSelect({
  label,
  value,
  choices,
  onChange
}: {
  label: string;
  value: readonly KeyToken[];
  choices: readonly (readonly KeyToken[])[];
  onChange: (value: KeyToken[]) => void;
}): JSX.Element {
  const { t } = useI18n();
  const separator = t(" + ", " + ");
  return (
    <div className="appearance-settings-page__shortcut-control">
      <select
        className="input"
        aria-label={label}
        value={shortcutValue(value)}
        onChange={(event) => {
          const selected = choices.find(
            (choice) => shortcutValue(choice) === event.target.value
          );
          if (selected) onChange([...selected]);
        }}
      >
        {choices.map((choice) => (
          <option key={shortcutValue(choice)} value={shortcutValue(choice)}>
            {shortcutKeyCaps(choice).join(separator)}
          </option>
        ))}
      </select>
      {/* Native options cannot contain kbd elements; render matching keycaps beside the select while retaining native keyboard behavior. */}
      <span className="appearance-settings-page__keycaps" aria-hidden="true">
        {shortcutKeyCaps(value).map((cap) => (
          <kbd key={cap}>{cap}</kbd>
        ))}
      </span>
    </div>
  );
}

function nextNonCollidingShortcut(
  binding: readonly KeyToken[]
): KeyToken[] {
  const available = COMPOSER_SHORTCUT_CHOICES.find(
    (choice) => !bindingsConflict(choice, binding)
  );
  return [...(available ?? ["Enter"])];
}

export function AppearanceSettings({
  settings,
  onChange
}: AppearanceSettingsProps): JSX.Element {
  const { t } = useI18n();
  const appearance = settings.appearance;
  const [colorDraft, setColorDraft] = useState(appearance.themeColor);
  const [fontSizeDraft, setFontSizeDraft] = useState(
    appearance.messageFontSize
  );
  const committedFontSizeRef = useRef(appearance.messageFontSize);

  useEffect(() => {
    setColorDraft(appearance.themeColor);
  }, [appearance.themeColor]);

  useEffect(() => {
    setFontSizeDraft(appearance.messageFontSize);
    committedFontSizeRef.current = appearance.messageFontSize;
  }, [appearance.messageFontSize]);

  const updateAppearance = (update: AppearanceUpdater): void => {
    onChange((current) => ({
      ...current,
      appearance: update(current.appearance)
    }));
  };

  const setAppearanceValue = <Key extends keyof AppearancePreferences>(
    key: Key,
    value: AppearancePreferences[Key]
  ): void => {
    updateAppearance((current) => ({ ...current, [key]: value }));
  };

  const setTheme = (theme: ThemePreference): void => {
    onChange((current) => ({ ...current, theme }));
  };

  const setLanguage = (appLanguage: AppLanguage): void => {
    onChange((current) => ({ ...current, appLanguage }));
  };

  const commitFontSize = (value: number): void => {
    const next = clampMessageFontSize(value);
    setFontSizeDraft(next);
    if (next === committedFontSizeRef.current) return;
    committedFontSizeRef.current = next;
    setAppearanceValue("messageFontSize", next);
  };

  const handleFontSizeChange = (event: ChangeEvent<HTMLInputElement>): void => {
    const next = clampMessageFontSize(Number(event.target.value));
    setFontSizeDraft(next);
    // React maps continuous range input to onChange. Only native change commits keyboard/non-pointer input; pointerup commits dragging to avoid persisting every pixel.
    if (event.nativeEvent.type === "change") commitFontSize(next);
  };

  const handleFontSizePointerUp = (
    event: PointerEvent<HTMLInputElement>
  ): void => {
    commitFontSize(Number(event.currentTarget.value));
  };

  const automaticLanguage = resolveApplicationLanguage("auto");
  const automaticLanguageLabel = automaticLanguage === "zh-CN"
    ? t("简体中文", "Simplified Chinese")
    : t("英语", "English");
  const normalizedThemeColor = normalizeHexColor(appearance.themeColor) ?? "";
  const colorPickerValue = normalizedThemeColor || THEME_COLOR_PRESETS[0];
  const themeChoices: Array<{
    value: ThemePreference;
    label: string;
    icon: typeof Sun;
  }> = [
    { value: "day", label: t("浅色", "Light"), icon: Sun },
    { value: "night", label: t("深色", "Dark"), icon: Moon },
    { value: "system", label: t("跟随系统", "Follow system"), icon: Monitor }
  ];
  const newlineChoices = COMPOSER_SHORTCUT_CHOICES.filter(
    (choice) => !bindingsConflict(choice, appearance.sendShortcut)
  );

  return (
    <div className="settings-page appearance-settings-page">
      <SettingsPageHeading
        title={t("外观", "Appearance")}
        description={t("主题、字体与消息渲染。", "Theme, fonts, and message rendering.")}
      />

      <SettingsCard title={t("主题", "Theme")}>
        <SettingRow title={t("主题", "Theme")} vertical>
          <div className="appearance-settings-page__theme-grid">
            {themeChoices.map((choice) => {
              const ThemeIcon = choice.icon;
              return (
                <button
                  key={choice.value}
                  type="button"
                  className="appearance-settings-page__theme-preview"
                  aria-pressed={settings.theme === choice.value}
                  onClick={() => setTheme(choice.value)}
                >
                  <span className="appearance-settings-page__theme-preview-surface">
                    <ThemeIcon size={19} />
                  </span>
                  <strong>{choice.label}</strong>
                </button>
              );
            })}
          </div>
        </SettingRow>
        <SettingRow title={t("主色", "Accent color")} vertical>
          <div className="appearance-settings-page__color-controls">
            <button
              type="button"
              className="appearance-settings-page__color-swatch appearance-settings-page__color-swatch--default"
              aria-label={t("默认主色", "Default accent color")}
              title={t("默认", "Default")}
              aria-pressed={appearance.themeColor === ""}
              onClick={() => {
                setColorDraft("");
                setAppearanceValue("themeColor", "");
              }}
            >
              <RotateCcw size={13} />
            </button>
            {THEME_COLOR_PRESETS.map((color) => (
              <button
                key={color}
                type="button"
                className="appearance-settings-page__color-swatch"
                style={{ backgroundColor: color }}
                aria-label={t("主色 {color}", "Accent color {color}", { color })}
                title={color}
                aria-pressed={normalizedThemeColor === color}
                onClick={() => {
                  setColorDraft(color);
                  setAppearanceValue("themeColor", color);
                }}
              />
            ))}
            <label className="appearance-settings-page__native-color">
              <span className="sr-only">{t("选择主色", "Choose accent color")}</span>
              <input
                type="color"
                aria-label={t("选择主色", "Choose accent color")}
                value={colorPickerValue}
                onChange={(event) => {
                  const next = normalizeHexColor(event.target.value);
                  if (next === null) return;
                  setColorDraft(next);
                  setAppearanceValue("themeColor", next);
                }}
              />
            </label>
            <input
              className="input appearance-settings-page__hex-input"
              aria-label={t("十六进制主色", "Hex accent color")}
              value={colorDraft}
              placeholder={t("默认", "Default")}
              spellCheck={false}
              onChange={(event) => setColorDraft(event.target.value)}
              onBlur={() => {
                const next = normalizeHexColor(colorDraft);
                if (next === null) {
                  setColorDraft(appearance.themeColor);
                  return;
                }
                setColorDraft(next);
                if (next !== appearance.themeColor) {
                  setAppearanceValue("themeColor", next);
                }
              }}
            />
          </div>
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("显示与语言", "Display and language")}>
        <SettingRow title={t("应用语言", "Application language")}>
          <select
            className="input"
            aria-label={t("应用语言", "Application language")}
            value={settings.appLanguage}
            onChange={(event) => setLanguage(event.target.value as AppLanguage)}
          >
            <option value="auto">
              {t("自动（{resolved}）", "Automatic ({resolved})", {
                resolved: automaticLanguageLabel
              })}
            </option>
            <option value="zh-CN">{t("简体中文", "Simplified Chinese")}</option>
            <option value="en-US">{t("英语", "English")}</option>
          </select>
        </SettingRow>
        <SettingRow title={t("页面缩放", "Page zoom")}>
          <div className="appearance-settings-page__zoom-controls">
            {appearance.zoom !== 1 && (
              <IconButton
                label={t("重置页面缩放", "Reset page zoom")}
                onClick={() => setAppearanceValue("zoom", 1)}
              >
                <RotateCcw size={15} />
              </IconButton>
            )}
            <IconButton
              label={t("缩小", "Zoom out")}
              disabled={appearance.zoom <= MIN_ZOOM}
              onClick={() =>
                setAppearanceValue("zoom", clampZoom(appearance.zoom - ZOOM_STEP))
              }
            >
              <Minus size={15} />
            </IconButton>
            <span className="appearance-settings-page__zoom-value">
              {t("{value}%", "{value}%", {
                value: Math.round(appearance.zoom * 100)
              })}
            </span>
            <IconButton
              label={t("放大", "Zoom in")}
              disabled={appearance.zoom >= MAX_ZOOM}
              onClick={() =>
                setAppearanceValue("zoom", clampZoom(appearance.zoom + ZOOM_STEP))
              }
            >
              <Plus size={15} />
            </IconButton>
          </div>
        </SettingRow>
        <SettingRow title={t("宽屏消息布局", "Wide message layout")}>
          <Switch
            label={t("宽屏消息布局", "Wide message layout")}
            checked={appearance.wideMessages}
            onChange={(checked) => setAppearanceValue("wideMessages", checked)}
          />
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("字体", "Fonts")}>
        <SettingRow title={t("界面字体", "Interface font")}>
          <FontCombobox
            label={t("界面字体", "Interface font")}
            value={appearance.uiFontFamily}
            presets={UI_FONT_PRESETS}
            onChange={(value) => setAppearanceValue("uiFontFamily", value)}
          />
        </SettingRow>
        <SettingRow title={t("等宽字体", "Monospace font")}>
          <FontCombobox
            label={t("等宽字体", "Monospace font")}
            value={appearance.monoFontFamily}
            presets={MONO_FONT_PRESETS}
            onChange={(value) => setAppearanceValue("monoFontFamily", value)}
          />
        </SettingRow>
        <SettingRow title={t("消息字号", "Message font size")} vertical>
          <div className="appearance-settings-page__font-size-control">
            <input
              type="range"
              aria-label={t("消息字号", "Message font size")}
              min={MIN_MESSAGE_FONT_SIZE}
              max={MAX_MESSAGE_FONT_SIZE}
              step={1}
              value={fontSizeDraft}
              onInput={(event) =>
                setFontSizeDraft(
                  clampMessageFontSize(Number(event.currentTarget.value))
                )
              }
              onChange={handleFontSizeChange}
              onPointerUp={handleFontSizePointerUp}
            />
            <div className="appearance-settings-page__font-size-ticks" aria-hidden="true">
              <span>{t("A", "A")}</span>
              <span>{t("默认", "Default")}</span>
              <span>{t("A", "A")}</span>
            </div>
          </div>
        </SettingRow>
        <SettingRow title={t("使用衬线字体", "Use serif font")}>
          <Switch
            label={t("使用衬线字体", "Use serif font")}
            checked={appearance.serifMessages}
            onChange={(checked) => setAppearanceValue("serifMessages", checked)}
          />
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("输入", "Input")}>
        <SettingRow title={t("发送快捷键", "Send shortcut")}>
          <ShortcutSelect
            label={t("发送快捷键", "Send shortcut")}
            value={appearance.sendShortcut}
            choices={COMPOSER_SHORTCUT_CHOICES}
            onChange={(sendShortcut) => {
              updateAppearance((current) => ({
                ...current,
                sendShortcut,
                newlineShortcut: bindingsConflict(
                  sendShortcut,
                  current.newlineShortcut
                )
                  ? nextNonCollidingShortcut(sendShortcut)
                  : current.newlineShortcut
              }));
            }}
          />
        </SettingRow>
        <SettingRow title={t("换行快捷键", "Newline shortcut")}>
          <ShortcutSelect
            label={t("换行快捷键", "Newline shortcut")}
            value={appearance.newlineShortcut}
            choices={newlineChoices}
            onChange={(candidate) => {
              const newlineShortcut = bindingsConflict(
                candidate,
                appearance.sendShortcut
              )
                ? nextNonCollidingShortcut(appearance.sendShortcut)
                : candidate;
              setAppearanceValue("newlineShortcut", newlineShortcut);
            }}
          />
        </SettingRow>
        <SettingRow title={t("拼写检查", "Spell check")}>
          <Switch
            label={t("拼写检查", "Spell check")}
            checked={appearance.spellCheck}
            onChange={(checked) => setAppearanceValue("spellCheck", checked)}
          />
        </SettingRow>
        <SettingRow
          title={t(
            "用 Markdown 渲染用户消息",
            "Render user messages with Markdown"
          )}
        >
          <Switch
            label={t(
              "用 Markdown 渲染用户消息",
              "Render user messages with Markdown"
            )}
            checked={appearance.renderUserMarkdown}
            onChange={(checked) =>
              setAppearanceValue("renderUserMarkdown", checked)
            }
          />
        </SettingRow>
        <SettingRow title={t("删除消息前确认", "Confirm before deleting messages")}>
          <Switch
            label={t("删除消息前确认", "Confirm before deleting messages")}
            checked={appearance.confirmMessageDelete}
            onChange={(checked) =>
              setAppearanceValue("confirmMessageDelete", checked)
            }
          />
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("消息", "Messages")}>
        <SettingRow title={t("折叠思维链", "Collapse reasoning")}>
          <Switch
            label={t("折叠思维链", "Collapse reasoning")}
            checked={appearance.collapseReasoning}
            onChange={(checked) =>
              setAppearanceValue("collapseReasoning", checked)
            }
          />
        </SettingRow>
        <SettingRow title={t("代码块可折叠", "Collapsible code blocks")}>
          <Switch
            label={t("代码块可折叠", "Collapsible code blocks")}
            checked={appearance.codeBlockCollapsible}
            onChange={(checked) =>
              setAppearanceValue("codeBlockCollapsible", checked)
            }
          />
        </SettingRow>
        <SettingRow title={t("代码块自动换行", "Wrap code blocks")}>
          <Switch
            label={t("代码块自动换行", "Wrap code blocks")}
            checked={appearance.codeBlockWrappable}
            onChange={(checked) =>
              setAppearanceValue("codeBlockWrappable", checked)
            }
          />
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("数学", "Math")}>
        <SettingRow
          title={t("启用 $...$ 行内公式", "Enable $...$ inline math")}
          description={t(
            "关闭后只渲染 $$…$$ 公式。",
            "When off, only $$…$$ formulas are rendered."
          )}
        >
          <Switch
            label={t("启用 $...$ 行内公式", "Enable $...$ inline math")}
            checked={appearance.singleDollarMath}
            onChange={(checked) =>
              setAppearanceValue("singleDollarMath", checked)
            }
          />
        </SettingRow>
      </SettingsCard>

      <SettingsCard title={t("自定义 CSS", "Custom CSS")}>
        <SettingRow
          title={t("样式表", "Stylesheet")}
          description={t(
            "通过 constructable stylesheet 应用，可在严格 CSP 下持续生效。",
            "Applied through a constructable stylesheet so it survives the app's strict CSP."
          )}
          vertical
        >
          <textarea
            className="input appearance-settings-page__custom-css"
            aria-label={t("自定义 CSS", "Custom CSS")}
            placeholder={t(
              "/* 在这里输入自定义 CSS */",
              "/* Put custom CSS here */"
            )}
            spellCheck={false}
            value={appearance.customCss}
            onChange={(event) =>
              setAppearanceValue("customCss", event.target.value)
            }
          />
        </SettingRow>
      </SettingsCard>
    </div>
  );
}
