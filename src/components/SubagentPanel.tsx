import {
  ArrowLeft,
  Bot,
  ChevronLeft,
  CircleAlert,
  CircleCheck,
  CircleSlash,
  CircleX,
  LoaderCircle,
} from "lucide-react";
import { useId } from "react";
import { useI18n } from "../i18n";
import type { ToolDescriptor } from "../types";
import { subagentAvatarTone } from "../lib/subagents";
import type { SubagentView, SubagentViewStatus } from "../lib/subagents";
import { ConversationView } from "./ConversationView";
import "./SubagentPanel.css";

export interface SubagentPanelProps {
  agent: SubagentView;
  tools?: ToolDescriptor[];
  /** Whole flat agent tree, so a search-group page can walk to its neighbours. */
  agents?: SubagentView[];
  onSelectAgent?: (id: string) => void;
  /** Leaves the transcript and returns the main area to the conversation. */
  onClose?: () => void;
}

const statusMeta: Record<SubagentViewStatus, {
  label: (t: ReturnType<typeof useI18n>["t"]) => string;
  icon: typeof CircleCheck;
}> = {
  running: { label: (t) => t("工作中", "Working"), icon: LoaderCircle },
  completed: { label: (t) => t("已完成", "Completed"), icon: CircleCheck },
  interrupted: { label: (t) => t("已中断", "Interrupted"), icon: CircleSlash },
  failed: { label: (t) => t("已失败", "Failed"), icon: CircleX },
  stopped: { label: (t) => t("已停止", "Stopped"), icon: CircleSlash },
  roundLimit: { label: (t) => t("已达轮次上限", "Round limit reached"), icon: CircleAlert }
};

function SubagentAvatar({ agent, compact = false }: { agent: SubagentView; compact?: boolean }) {
  return (
    <span
      className={`subagent-avatar subagent-avatar--${subagentAvatarTone(agent.id)}${compact ? " subagent-avatar--compact" : ""}`}
      title={agent.label}
      aria-hidden="true"
    >
      <Bot size={compact ? 12 : 15} />
    </span>
  );
}

function AgentStatus({ status }: { status: SubagentViewStatus }) {
  const { t } = useI18n();
  const meta = statusMeta[status];
  const Icon = meta.icon;
  return (
    <span className={`subagent-status subagent-status--${status}`}>
      <Icon className={status === "running" ? "subagent-status__spinner" : undefined} size={12} aria-hidden="true" />
      {meta.label(t)}
    </span>
  );
}

/** Parent link and direct children of one agent, so a search group reads as a
 * tree instead of a flat list of unrelated threads. */
function SubagentGroupNav({
  agent,
  agents,
  onSelectAgent
}: {
  agent: SubagentView;
  agents: SubagentView[];
  onSelectAgent: (id: string) => void;
}) {
  const { t } = useI18n();
  const owner = agent.parentId
    ? agents.find((view) => view.id === agent.parentId) ?? null
    : null;
  // A workflow step's parent is the run, and a run is a script: it has no
  // transcript to go back to. The way out of a step is the run's own panel,
  // which is already open in the task container behind this one.
  const parent = owner && !owner.workflowRun ? owner : null;

  const children = agent.childIds.flatMap((id) => {
    const child = agents.find((view) => view.id === id);
    return child ? [child] : [];
  });
  if (!parent && !children.length) return null;
  return (
    <nav className="subagent-panel__group" aria-label={t("子代理组结构", "Subagent group structure")}>
      {parent && (
        <button
          type="button"
          className="subagent-panel__group-parent"
          aria-label={t("返回上级子代理 {label}", "Back to parent subagent {label}", { label: parent.label })}
          onClick={() => onSelectAgent(parent.id)}
        >
          <ChevronLeft size={12} aria-hidden="true" />
          <span>{parent.label}</span>
        </button>
      )}
      {children.map((child) => (
        <button
          key={child.id}
          type="button"
          className={`subagent-panel__group-child subagent-panel__group-child--${child.status}`}
          aria-label={t("打开子代理 {label}", "Open subagent {label}", { label: child.label })}
          onClick={() => onSelectAgent(child.id)}
        >
          <SubagentAvatar agent={child} compact />
          <span>{child.label}</span>
          <AgentStatus status={child.status} />
        </button>
      ))}
    </nav>
  );
}

export function SubagentPanel({
  agent,
  tools = [],
  agents = [],
  onSelectAgent,
  onClose
}: SubagentPanelProps) {
  const { t } = useI18n();
  const titleId = useId();

  return (
    <section
      className="subagent-panel-content"
      aria-labelledby={titleId}
    >
      <header className="subagent-panel__header">
        {onClose ? (
          <button
            type="button"
            className="subagent-panel__icon-button"
            aria-label={t("返回对话", "Back to the conversation")}
            onClick={onClose}
          >
            <ArrowLeft size={16} aria-hidden="true" />
          </button>
        ) : (
          <Bot size={17} aria-hidden="true" />
        )}
        <div>
          <span>{t("只读终端", "Read-only terminal")}</span>
          <h2 id={titleId}>{agent.label}</h2>
        </div>
        <AgentStatus status={agent.status} />
      </header>
      {onSelectAgent && (
        <SubagentGroupNav agent={agent} agents={agents} onSelectAgent={onSelectAgent} />
      )}
      <div className="subagent-panel__body">
        <ConversationView
          contexts={agent.contexts}
          tools={tools}
          enabledTools={tools.map((tool) => tool.name)}
          editable={false}
          streaming={agent.status === "running"}
          ariaLabel={t("{label}只读终端", "{label} read-only terminal", { label: agent.label })}
        />
      </div>
    </section>
  );
}
