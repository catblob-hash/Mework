import {
  ChevronRight,
  GitBranch,
  GitCommitHorizontal,
  GitCompareArrows,
  Laptop
} from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { GitReviewView, GitWorkspaceSnapshot } from "../lib/git";
import { mainPanePageDomId, mainPaneViewKey } from "../lib/mainPanePages";

/** The review page these rows drive. One page whose view changes, so one id for all four. */
const reviewPageDomId = mainPanePageDomId(mainPaneViewKey({ kind: "review", view: "changes" }));
import "./GitStatusCard.css";

export interface GitStatusCardProps {
  /**
   * The workspace's repository. The card exists only to report it, so the
   * caller renders nothing at all when the workspace is not a Git working
   * directory rather than passing null.
   */
  git: GitWorkspaceSnapshot;
  gitOpen?: boolean;
  gitView?: GitReviewView | null;
  onOpenGitReview?: (view: GitReviewView) => void;
}

export function GitStatusCard({
  git,
  gitOpen = false,
  gitView = null,
  onOpenGitReview = () => undefined
}: GitStatusCardProps) {
  const { t } = useI18n();
  const cardBodyId = useId();
  const cardRef = useRef<HTMLElement>(null);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (!expanded) return;
    const closeOnOutsidePress = (event: MouseEvent) => {
      if (!cardRef.current?.contains(event.target as Node)) setExpanded(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented) setExpanded(false);
    };
    document.addEventListener("mousedown", closeOnOutsidePress);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", closeOnOutsidePress);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [expanded]);

  const changeCount = git.changedFiles
    ?? (git.files.length || git.staged + git.unstaged + git.untracked + git.conflicted);
  const operationAction = git.operation === "merge"
    ? t("继续合并", "Continue merge")
    : git.operation === "rebase"
      ? t("继续变基", "Continue rebase")
      : git.operation === "cherryPick"
        ? t("继续拣选提交", "Continue cherry-pick")
        : git.operation === "revert"
          ? t("继续还原提交", "Continue revert")
          : git.operation === "bisect"
            ? t("查看二分查找", "Review bisect")
            : null;
  const primaryAction = git.conflicted
    ? t("解决 {count} 个冲突", "Resolve {count} conflicts", { count: git.conflicted })
    : operationAction
      ? operationAction
      : changeCount > 0
        ? t("提交更改", "Commit changes")
        : git.ahead > 0
          ? t("推送 {count} 个提交", "Push {count} commits", { count: git.ahead })
          : git.behind > 0
            ? t("拉取 {count} 个提交", "Pull {count} commits", { count: git.behind })
            : t("提交或推送", "Commit or push");
  const primaryView: GitReviewView = changeCount > 0 || git.conflicted > 0 || git.operation
    ? "changes"
    : "history";

  return (
    <aside
      ref={cardRef}
      className={`git-status-card${expanded ? " git-status-card--expanded" : ""}`}
      aria-label={t("Git 状态", "Git status")}
    >
      <button
        type="button"
        className="git-status-card__toggle conversation-overview__text-action"
        aria-label={expanded
          ? t("收起 Git 状态卡片", "Collapse Git status card")
          : t("展开 Git 状态卡片", "Expand Git status card")}
        aria-expanded={expanded}
        aria-controls={cardBodyId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span>{t("Git 状态", "Git status")}</span>
      </button>
      <div id={cardBodyId} className="git-status-card__body">
        <div className="git-status-card__heading">{t("Git 状态", "Git status")}</div>
        <button
          type="button"
          className={`git-status-card__row${gitOpen && gitView === "changes" ? " git-status-card__row--open" : ""}`}
          aria-current={gitOpen && gitView === "changes" ? "page" : undefined}
          aria-controls={reviewPageDomId}
          onClick={() => onOpenGitReview("changes")}
        >
          <GitCompareArrows size={14} aria-hidden="true" />
          <strong>{t("变更", "Changes")}</strong>
          <span className="git-status-card__diff-stat" aria-label={t(
            "新增 {additions} 行，删除 {deletions} 行",
            "{additions} lines added, {deletions} lines deleted",
            { additions: git.additions, deletions: git.deletions }
          )}>
            <b>+{git.additions.toLocaleString()}</b>
            <em>−{git.deletions.toLocaleString()}</em>
          </span>
        </button>
        <div className="git-status-card__row git-status-card__row--static">
          <Laptop size={14} aria-hidden="true" />
          <strong>{t("本地", "Local")}</strong>
          <small>{git.remote?.name ?? t("无远程仓库", "No remote")}</small>
        </div>
        <button
          type="button"
          className={`git-status-card__row${gitOpen && gitView === "branches" ? " git-status-card__row--open" : ""}`}
          aria-current={gitOpen && gitView === "branches" ? "page" : undefined}
          aria-controls={reviewPageDomId}
          onClick={() => onOpenGitReview("branches")}
        >
          <GitBranch size={14} aria-hidden="true" />
          <strong title={git.branch ?? git.head ?? ""}>
            {git.branch ?? (git.head
              ? t("分离头指针 {head}", "Detached at {head}", { head: git.head })
              : t("尚无提交", "No commits yet"))}
          </strong>
          {(git.ahead > 0 || git.behind > 0) && (
            <small>{git.ahead > 0 ? `↑${git.ahead}` : ""}{git.ahead > 0 && git.behind > 0 ? " " : ""}{git.behind > 0 ? `↓${git.behind}` : ""}</small>
          )}
          <ChevronRight size={14} aria-hidden="true" />
        </button>
        <button
          type="button"
          className={`git-status-card__row${gitOpen && primaryView !== "changes" && gitView === primaryView ? " git-status-card__row--open" : ""}`}
          aria-current={gitOpen && primaryView !== "changes" && gitView === primaryView ? "page" : undefined}
          aria-controls={reviewPageDomId}
          onClick={() => onOpenGitReview(primaryView)}
        >
          <GitCommitHorizontal size={14} aria-hidden="true" />
          <strong>{primaryAction}</strong>
          <ChevronRight size={14} aria-hidden="true" />
        </button>
        <button
          type="button"
          className={`git-status-card__row${gitOpen && gitView === "compare" ? " git-status-card__row--open" : ""}`}
          aria-current={gitOpen && gitView === "compare" ? "page" : undefined}
          aria-controls={reviewPageDomId}
          onClick={() => onOpenGitReview("compare")}
        >
          <GitCompareArrows size={14} aria-hidden="true" />
          <strong>{t("比较分支", "Compare branches")}</strong>
          <ChevronRight size={14} aria-hidden="true" />
        </button>
      </div>
    </aside>
  );
}
