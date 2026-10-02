import type { ComponentProps } from "react";
import { useI18n } from "../i18n";
import type { LockTone } from "../lib/toolLock";
import { Switch } from "./Common";
import { LockableSwitchRow, type LockHints } from "./LockTone";
import { ToolDescriptionSelectRow } from "./PresetComposition";
import { WebSearchBehaviorSettings } from "./WebSearchBehaviorSettings";
import type { WebSearchBehaviorSettingsProps } from "./WebSearchBehaviorSettings";

interface AdvancedToolsPageProps {
  /** The search and fetch backends, with the shaping and filtering under them. */
  web: WebSearchBehaviorSettingsProps;
  /**
   * The switch deciding whether this surface reaches the web at all. Omitted
   * where that decision belongs to someone else — a role's caller decides it —
   * and the backend rows are then always drawn, since they are all that is left
   * to answer.
   */
  webAccess?: {
    enabled: boolean;
    tone?: LockTone | null;
    onChange: (enabled: boolean) => void;
  };
  /** The two memory tiers. Omitted where memory is not a switch of this surface. */
  memory?: {
    global: boolean;
    project: boolean;
    globalTone?: LockTone | null;
    projectTone?: LockTone | null;
    onChangeGlobal: (enabled: boolean) => void;
    onChangeProject: (enabled: boolean) => void;
  };
  /**
   * Whether the app-data path joins the system prompt. Conversation-only, so a
   * preset body has no room to carry it: drawing the switch there would promise
   * a save that discards it.
   */
  appDataPath?: {
    enabled: boolean;
    onChange: (enabled: boolean) => void;
  };
  /** Which tool-description profile renders the prompt. */
  toolDescription?: ComponentProps<typeof ToolDescriptionSelectRow>;
  /** What a row the conversation's lock tones says about itself. */
  lockHints?: LockHints;
}

/**
 * The advanced tools page: what a model is handed through a switch rather than
 * picked row by row from the tool list, which has a page of its own.
 *
 * One page for every surface that asks these questions — a conversation, a
 * preset opened in its window, and a subagent role — so a row added or
 * reworded here lands on all of them at once. A surface that cannot answer a
 * section leaves that section's prop out, and the section is not drawn: that
 * is the only way the three differ.
 */
export function AdvancedToolsPage({
  web,
  webAccess,
  memory,
  appDataPath,
  toolDescription,
  lockHints
}: AdvancedToolsPageProps) {
  const { t } = useI18n();

  return (
    <>
      {/* Web access is one switch, not two tool checkboxes. Upstreams do not
          agree on how many web tools there are — Anthropic exposes search
          and fetch separately, DeepSeek and OpenAI expose search alone and
          keep page retrieval inside it — so the host derives the pair from
          this switch and the resolved backend rather than letting the picker
          promise a shape the upstream may not have. */}
      <section className="conversation-settings__field">
        {webAccess && (
          <LockableSwitchRow
            title={t("启用联网搜索", "Enable web search")}
            description={t(
              "这个对话能不能联网。具体拿到哪几个联网工具，由下面的搜索后端与抓取后端各自决定——两者可以分别指定，也可以分别关掉。",
              "Whether this conversation can reach the web at all. Which web tools it actually gets is decided by the search and fetch backends below: each names its own, and each can be turned off on its own."
            )}
            checked={webAccess.enabled}
            tone={webAccess.tone}
            hints={lockHints}
            onChange={webAccess.onChange}
            label={webAccess.enabled
              ? t("联网搜索已开启", "Web search enabled")
              : t("联网搜索已关闭", "Web search disabled")}
          />
        )}
        {!webAccess || webAccess.enabled ? (
          <div className="web-search-provider-field">
            <WebSearchBehaviorSettings {...web} />
          </div>
        ) : null}
      </section>

      {(memory || appDataPath) && (
        <section className="conversation-settings__field">
          {memory && (
            <>
              <LockableSwitchRow
                title={t("启用全局记忆", "Enable global memory")}
                description={t(
                  "开启后，~/.mework 的 MEWORK.md 常驻指令与 MEMORY.md 记忆索引拼进上下文，读取/创建/编辑全局记忆三个工具随之可用。",
                  "When enabled, ~/.mework's MEWORK.md instructions and MEMORY.md index join the context, and the read/create/edit global memory tools become available."
                )}
                checked={memory.global}
                tone={memory.globalTone}
                hints={lockHints}
                onChange={memory.onChangeGlobal}
                label={memory.global
                  ? t("全局记忆已开启", "Global memory enabled")
                  : t("全局记忆已关闭", "Global memory disabled")}
              />
              <LockableSwitchRow
                title={t("启用项目记忆", "Enable project memory")}
                description={t(
                  "开启后，当前工作区 .mework 的 MEWORK.md 与 MEMORY.md 拼进上下文，读取/创建/编辑项目记忆三个工具随之可用。",
                  "When enabled, this workspace's .mework MEWORK.md and MEMORY.md join the context, and the read/create/edit project memory tools become available."
                )}
                checked={memory.project}
                tone={memory.projectTone}
                hints={lockHints}
                onChange={memory.onChangeProject}
                label={memory.project
                  ? t("项目记忆已开启", "Project memory enabled")
                  : t("项目记忆已关闭", "Project memory disabled")}
              />
            </>
          )}
          {appDataPath && (
            <div className="tool-toggle-row">
              <span><strong>{t("拼接应用数据目录", "Include app data directory")}</strong><small>{t(
                "开启后，模型会在系统提示词中看到受信任的应用数据目录绝对路径。",
                "When enabled, the model sees the trusted app data directory's absolute path in the system prompt."
              )}</small></span>
              <Switch
                checked={appDataPath.enabled}
                onChange={appDataPath.onChange}
                label={appDataPath.enabled
                  ? t("拼接应用数据目录已开启", "App data directory enabled")
                  : t("拼接应用数据目录已关闭", "App data directory disabled")}
              />
            </div>
          )}
        </section>
      )}

      {/* The five race-safe write guards used to be five switches
          here. They are unconditional now — every conversation runs
          with all five — so the section is gone rather than drawn as
          a row of controls nothing can move. */}

      {toolDescription && (
        <section className="conversation-settings__field">
          <ToolDescriptionSelectRow {...toolDescription} />
        </section>
      )}
    </>
  );
}
