import { render, screen, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../../i18n";
import type { ApiProvider, CodexOauthStatus } from "../../types";

const runtimeMocks = vi.hoisted(() => ({
  codexOauthStatus: vi.fn(),
  codexOauthSignIn: vi.fn(),
  codexOauthCancelSignIn: vi.fn(),
  codexOauthSignOut: vi.fn(),
}));

vi.mock("../../lib/runtime", () => runtimeMocks);

import { CodexLoginPanel } from "./CodexLoginPanel";

const signedOut: CodexOauthStatus = { signedIn: false, signingIn: false, account: null };

function provider(): ApiProvider {
  return {
    id: "provider_codex",
    name: "OpenAI Codex",
    enabled: false,
    family: "openai_codex",
    baseUrl: "",
    familySettings: {},
    endpointBaseUrls: {},
    notes: "",
    models: [],
    activeModelId: null,
  };
}

function renderPanel(overrides: Partial<ComponentProps<typeof CodexLoginPanel>> = {}) {
  const onSignedInChange = vi.fn();
  return {
    onSignedInChange,
    ...render(<CodexLoginPanel
      provider={provider()}
      desktopRuntime
      onSignedInChange={onSignedInChange}
      {...overrides}
    />),
  };
}

afterEach(() => configureI18n("zh-CN"));

describe("CodexLoginPanel", () => {
  beforeEach(() => {
    configureI18n("en-US");
    runtimeMocks.codexOauthStatus.mockReset().mockResolvedValue(signedOut);
    runtimeMocks.codexOauthSignIn.mockReset();
    runtimeMocks.codexOauthCancelSignIn.mockReset().mockResolvedValue(undefined);
    runtimeMocks.codexOauthSignOut.mockReset();
  });

  it("renders the sign-in button while signed out", async () => {
    renderPanel();
    expect(await screen.findByRole("button", { name: "Sign in with ChatGPT" })).toBeEnabled();
  });

  it("shows the signed-in account email and plan", async () => {
    runtimeMocks.codexOauthStatus.mockResolvedValue({
      signedIn: true,
      signingIn: false,
      account: { accountId: "acct_123", email: "person@example.com", planType: "plus" },
    });
    renderPanel();

    expect(await screen.findByText("Signed in to ChatGPT")).toBeInTheDocument();
    expect(screen.getByText(/person@example\.com/)).toHaveTextContent("Plus");
    expect(screen.getByText("acct_123")).toHaveTextContent("acct_123");
  });

  it("signs in and enables the provider after authorization", async () => {
    const user = userEvent.setup();
    runtimeMocks.codexOauthSignIn.mockResolvedValue({
      signedIn: true,
      signingIn: false,
      account: { accountId: "acct_123" },
    });
    const { onSignedInChange } = renderPanel();

    await user.click(await screen.findByRole("button", { name: "Sign in with ChatGPT" }));
    await waitFor(() => expect(runtimeMocks.codexOauthSignIn).toHaveBeenCalledWith(expect.objectContaining({ id: "provider_codex" })));
    await waitFor(() => expect(onSignedInChange).toHaveBeenCalledWith(true));
  });

  it("cancels a pending sign-in", async () => {
    const user = userEvent.setup();
    let rejectSignIn!: (reason: Error) => void;
    runtimeMocks.codexOauthSignIn.mockReturnValue(new Promise((_, reject) => { rejectSignIn = reject; }));
    renderPanel();

    await user.click(await screen.findByRole("button", { name: "Sign in with ChatGPT" }));
    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(runtimeMocks.codexOauthCancelSignIn).toHaveBeenCalledWith(expect.objectContaining({ id: "provider_codex" })));
    rejectSignIn(new Error("Codex 登录已取消"));
  });

  it("signs out and disables the provider", async () => {
    const user = userEvent.setup();
    runtimeMocks.codexOauthStatus.mockResolvedValue({
      signedIn: true,
      signingIn: false,
      account: { accountId: "acct_123" },
    });
    runtimeMocks.codexOauthSignOut.mockResolvedValue(signedOut);
    const { onSignedInChange } = renderPanel();

    await user.click(await screen.findByRole("button", { name: "Sign out" }));
    await waitFor(() => expect(runtimeMocks.codexOauthSignOut).toHaveBeenCalledWith(expect.objectContaining({ id: "provider_codex" })));
    await waitFor(() => expect(onSignedInChange).toHaveBeenCalledWith(false));
  });

  it("renders sign-in failures except a user cancellation", async () => {
    const user = userEvent.setup();
    runtimeMocks.codexOauthSignIn.mockRejectedValueOnce(new Error("OAuth failed"));
    const first = renderPanel();
    await user.click(await screen.findByRole("button", { name: "Sign in with ChatGPT" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("OAuth failed");
    first.unmount();

    runtimeMocks.codexOauthSignIn.mockRejectedValueOnce(new Error("Codex 登录已取消"));
    renderPanel();
    await user.click(await screen.findByRole("button", { name: "Sign in with ChatGPT" }));
    await waitFor(() => expect(runtimeMocks.codexOauthSignIn).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  /// The renderer that started a sign-in may reload before it finishes; the
  /// panel then only sees the host's status flip. That flip must enable the
  /// provider exactly like a completed sign-in call, and the poll must stop.
  it("enables the provider when a polled sign-in completes after a reload", async () => {
    vi.useFakeTimers();
    try {
      const waiting: CodexOauthStatus = { signedIn: false, signingIn: true, account: null };
      const done: CodexOauthStatus = { signedIn: true, signingIn: false, account: { accountId: "acct_polled" } };
      runtimeMocks.codexOauthStatus.mockResolvedValueOnce(waiting).mockResolvedValue(done);
      const { onSignedInChange } = renderPanel();

      await vi.advanceTimersByTimeAsync(0);
      expect(runtimeMocks.codexOauthStatus).toHaveBeenCalledTimes(1);
      expect(onSignedInChange).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(2_000);
      expect(runtimeMocks.codexOauthStatus).toHaveBeenCalledTimes(2);
      expect(onSignedInChange).toHaveBeenCalledWith(true);

      // Signed in: no further polls are scheduled.
      await vi.advanceTimersByTimeAsync(10_000);
      expect(runtimeMocks.codexOauthStatus).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  /// An already-signed-in status on first load is not a transition: the user
  /// may have disabled the row deliberately, so the panel must not re-enable it.
  it("does not touch the enabled flag for a provider that was already signed in", async () => {
    runtimeMocks.codexOauthStatus.mockResolvedValue({ signedIn: true, signingIn: false, account: { accountId: "acct_123" } });
    const { onSignedInChange } = renderPanel();
    expect(await screen.findByText("Signed in to ChatGPT")).toBeInTheDocument();
    expect(onSignedInChange).not.toHaveBeenCalled();
  });

  it("flushes pending document edits before every host call", async () => {
    const user = userEvent.setup();
    const onBeforeHostCall = vi.fn().mockResolvedValue(undefined);
    runtimeMocks.codexOauthSignIn.mockResolvedValue({ signedIn: true, signingIn: false, account: { accountId: "acct_123" } });
    renderPanel({ onBeforeHostCall });
    await screen.findByRole("button", { name: "Sign in with ChatGPT" });
    expect(onBeforeHostCall).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "Sign in with ChatGPT" }));
    await waitFor(() => expect(runtimeMocks.codexOauthSignIn).toHaveBeenCalledTimes(1));
    expect(onBeforeHostCall).toHaveBeenCalledTimes(2);
    expect(onBeforeHostCall.mock.invocationCallOrder[1]).toBeLessThan(
      runtimeMocks.codexOauthSignIn.mock.invocationCallOrder[0]
    );
  });

  it("offers a retry when the first status read fails", async () => {
    const user = userEvent.setup();
    runtimeMocks.codexOauthStatus.mockRejectedValueOnce(new Error("bridge down")).mockResolvedValue(signedOut);
    renderPanel();
    expect(await screen.findByRole("alert")).toHaveTextContent("bridge down");
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("button", { name: "Sign in with ChatGPT" })).toBeEnabled();
  });
});
