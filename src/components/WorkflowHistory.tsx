import { useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { ContextItem, SubagentRunRecord, ToolDescriptor } from "../types";
import { workflowRunHistory, workflowStepRecord } from "../lib/runtime";
import { unrepresentedWorkflowHistory } from "../lib/workflowRuns";
import type { WorkflowHistoryRun } from "../lib/workflowRuns";
import { ConversationView } from "./ConversationView";

export interface WorkflowHistoryProps {
  conversationId: string;
  contexts: ContextItem[];
  representedCallIds: string[];
  liveCallIds?: string[];
  liveRunIds?: string[];
  tools?: ToolDescriptor[];
}

export function WorkflowHistory({ conversationId, contexts, representedCallIds, liveCallIds = [], liveRunIds = [], tools = [] }: WorkflowHistoryProps) {
  const { t } = useI18n();
  const [loaded, setLoaded] = useState<{ conversationId: string; runs: WorkflowHistoryRun[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [selection, setSelection] = useState<{ conversationId: string; runId: string; index: number; label: string; record: SubagentRunRecord | null } | null>(null);
  const [loadingBody, setLoadingBody] = useState(false);
  const requestSequence = useRef(0);
  useEffect(() => {
    let cancelled = false;
    requestSequence.current += 1;
    setError(null);
    setSelection(null);
    setLoadingBody(false);
    void workflowRunHistory(conversationId).then((runs) => {
      if (!cancelled) setLoaded({ conversationId, runs });
    }).catch((reason) => {
      if (!cancelled) setError(String(reason));
    });
    return () => { cancelled = true; requestSequence.current += 1; };
  }, [conversationId, retry]);
  const runs = unrepresentedWorkflowHistory(loaded?.conversationId === conversationId ? loaded.runs : [], contexts, representedCallIds, liveCallIds, liveRunIds);
  const selected = selection?.conversationId === conversationId ? selection : null;
  const labels = {
    completed: t("已完成", "Completed"),
    failed: t("已失败或中断", "Failed or interrupted"),
    cached: t("缓存结果", "Cached result"),
    unrecorded: t("未落盘", "Not recorded")
  };
  async function openStep(runId: string, index: number, label: string) {
    const sequence = ++requestSequence.current;
    setError(null);
    setLoadingBody(true);
    setSelection({ conversationId, runId, index, label, record: null });
    try {
      const record = await workflowStepRecord(conversationId, runId, index);
      if (requestSequence.current !== sequence) return;
      if (!record || !Array.isArray(record.contexts)) throw new Error(t("步骤正文已不存在或已损坏", "The step body is missing or damaged"));
      setSelection({ conversationId, runId, index, label, record });
    } catch (reason) {
      if (requestSequence.current === sequence) setError(String(reason));
    } finally {
      if (requestSequence.current === sequence) setLoadingBody(false);
    }
  }
  if (!runs.length && !error) return null;
  return (
    <section className="workflow-history" aria-label={t("历史工作流（只读）", "Workflow history (read-only)")}>
      <h3>{t("历史工作流（只读）", "Workflow history (read-only)")}</h3>
      <p>{t("从磁盘恢复的历史快照，不代表任务仍在运行。", "Historical snapshots recovered from disk; these do not indicate a live task.")}</p>
      {error && <div role="alert">{error}<button type="button" onClick={() => setRetry((value) => value + 1)}>{t("重新读取历史", "Reload history")}</button></div>}
      {runs.map((run) => (
        <details key={run.runId}>
          <summary><span>{run.scriptName}</span> <code>{run.runId}</code></summary>
          <p>{t("保存的状态：{status}", "Recorded status: {status}", { status: run.status })}</p>
          {run.steps.length === 0 && <p>{t("没有已保存的步骤记录", "No saved step records")}</p>}
          <ul>
            {run.steps.map((step) => (
              <li key={step.index}>
                <button type="button" disabled={!step.bodyAvailable} onClick={() => void openStep(run.runId, step.index, step.label)}>
                  {step.index + 1}. {step.label} — {labels[step.state]}
                </button>
                {!step.bodyAvailable && <span> {t("正文不可用", "Body unavailable")}</span>}
                {step.error && <p>{step.error}</p>}
              </li>
            ))}
          </ul>
          {selected?.runId === run.runId && (
            <section aria-label={t("历史步骤 {label}", "Archived step {label}", { label: selected.label })}>
              {loadingBody && <p>{t("正在读取步骤正文", "Loading step body")}</p>}
              {selected.record && <ConversationView contexts={selected.record.contexts} tools={tools} enabledTools={[]} editable={false} streaming={false} />}
            </section>
          )}
        </details>
      ))}
    </section>
  );
}
