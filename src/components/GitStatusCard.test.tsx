import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { GitWorkspaceSnapshot } from "../lib/git";
import { GitStatusCard } from "./GitStatusCard";

function snapshot(overrides: Partial<GitWorkspaceSnapshot> = {}): GitWorkspaceSnapshot {
  return {
    repositoryId: "repository-id-1",
    worktreeId: "worktree-id-1",
    branch: "main",
    head: "24cc318b",
    contentRevision: "revision-1",
    upstream: "origin/main",
    upstreamTarget: {
      remoteName: "origin",
      remoteBranch: "main",
      mergeRef: "refs/heads/main",
      trackingRef: "refs/remotes/origin/main",
      trackingOid: "24cc318b24cc318b24cc318b24cc318b24cc318b",
      isLocal: false,
      remote: {
        name: "origin",
        fetchRevision: "origin-fetch-revision-1",
        pushRevision: "origin-push-revision-1",
        url: "https://github.com/example-org/Mework.git"
      }
    },
    ahead: 2,
    behind: 1,
    additions: 34_466,
    deletions: 2_083,
    staged: 2,
    unstaged: 4,
    untracked: 1,
    conflicted: 0,
    stash: 0,
    files: [],
    remote: {
      name: "origin",
      fetchRevision: "origin-fetch-revision-1",
      pushRevision: "origin-push-revision-1",
      url: "https://github.com/example-org/Mework.git"
    },
    remotes: [{
      name: "origin",
      fetchRevision: "origin-fetch-revision-1",
      pushRevision: "origin-push-revision-1",
      url: "https://github.com/example-org/Mework.git"
    }],
    gitVersion: "2.54.0",
    detached: false,
    unborn: false,
    operation: null,
    operationRevision: null,
    isClean: false,
    binaryFiles: 0,
    warnings: [],
    ...overrides
  };
}

function openCard() {
  fireEvent.click(screen.getByRole("button", { name: "展开 Git 状态卡片" }));
}

describe("GitStatusCard", () => {
  it("reports the repository and points its rows at the review pane", async () => {
    const user = userEvent.setup();
    const onOpenGitReview = vi.fn();
    const { rerender } = render(
      <GitStatusCard git={snapshot()} onOpenGitReview={onOpenGitReview} />
    );

    openCard();
    const card = screen.getByRole("complementary", { name: "Git 状态" });
    expect(within(card).getByLabelText("新增 34466 行，删除 2083 行")).toBeInTheDocument();
    expect(card).toHaveTextContent("变更");
    expect(card).toHaveTextContent("main");
    expect(card).toHaveTextContent("↑2 ↓1");
    expect(card).toHaveTextContent("提交更改");
    expect(card).toHaveTextContent("origin");

    // The branch and local rows are readouts; only the two review rows are buttons,
    // and the first button is the card's own disclosure.
    const actions = within(card).getAllByRole("button");
    expect(actions).toHaveLength(3);
    await user.click(actions[1]!);
    await user.click(actions[2]!);
    expect(onOpenGitReview.mock.calls).toEqual([[], []]);

    rerender(<GitStatusCard git={snapshot()} onOpenGitReview={onOpenGitReview} gitOpen />);
    expect(within(card).getByRole("button", { name: /变更/ })).toHaveAttribute("aria-current", "page");
    expect(within(card).getByRole("button", { name: /提交更改/ })).toHaveAttribute("aria-current", "page");
    expect(card.querySelectorAll('[aria-current="page"]')).toHaveLength(2);
  });

  it("offers to finish an in-progress operation before offering to commit", async () => {
    const user = userEvent.setup();
    const onOpenGitReview = vi.fn();
    render(
      <GitStatusCard
        git={snapshot({ operation: "rebase", conflicted: 0 })}
        onOpenGitReview={onOpenGitReview}
      />
    );

    openCard();
    await user.click(screen.getByRole("button", { name: /继续变基/ }));
    expect(onOpenGitReview).toHaveBeenCalled();
  });

  it("puts conflicts ahead of every other suggested action", () => {
    render(<GitStatusCard git={snapshot({ operation: "merge", conflicted: 3 })} />);

    openCard();
    expect(screen.getByRole("button", { name: /解决 3 个冲突/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /继续合并/ })).not.toBeInTheDocument();
  });

  it("still offers the review pane when the repository is clean", async () => {
    const user = userEvent.setup();
    const onOpenGitReview = vi.fn();
    render(
      <GitStatusCard
        git={snapshot({
          additions: 0, deletions: 0, staged: 0, unstaged: 0, untracked: 0,
          ahead: 0, behind: 0, isClean: true, changedFiles: 0
        })}
        onOpenGitReview={onOpenGitReview}
      />
    );

    openCard();
    await user.click(screen.getByRole("button", { name: /提交或推送/ }));
    expect(onOpenGitReview).toHaveBeenCalled();
  });

  it("names a detached head and a missing remote rather than showing nothing", () => {
    render(<GitStatusCard git={snapshot({ branch: null, remote: null })} />);

    openCard();
    const card = screen.getByRole("complementary", { name: "Git 状态" });
    expect(card).toHaveTextContent("分离头指针 24cc318b");
    expect(card).toHaveTextContent("无远程仓库");
  });

  it("toggles the overlay and closes it with Escape", () => {
    const { container } = render(<GitStatusCard git={snapshot()} />);

    const toggle = screen.getByRole("button", { name: "展开 Git 状态卡片" });
    expect(toggle).toHaveTextContent("Git 状态");
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-label", "收起 Git 状态卡片");
    expect(container.querySelector(".git-status-card")).toHaveClass("git-status-card--expanded");

    fireEvent.keyDown(document, { key: "Escape" });
    expect(toggle).toHaveAttribute("aria-label", "展开 Git 状态卡片");
    expect(container.querySelector(".git-status-card")).not.toHaveClass("git-status-card--expanded");
  });
});
