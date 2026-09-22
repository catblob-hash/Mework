import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { configureI18n } from "../i18n";

const decisionMocks = vi.hoisted(() => ({
  deleteDecisionApiKey: vi.fn(),
  getDecisionKeyStatus: vi.fn(),
  revealDecisionApiKey: vi.fn(),
  saveDecisionApiKey: vi.fn()
}));
const backendMocks = vi.hoisted(() => ({
  hasBackendRuntime: vi.fn(() => false)
}));

vi.mock("../lib/decisionProviders", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/decisionProviders")>()),
  ...decisionMocks
}));
vi.mock("../lib/backend", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/backend")>()),
  ...backendMocks
}));
import { DecisionProviderSettings } from "./DecisionProviderSettings";

afterEach(() => configureI18n("zh-CN"));

describe("DecisionProviderSettings", () => {
  beforeEach(() => {
    configureI18n("en-US");
    decisionMocks.deleteDecisionApiKey.mockReset().mockResolvedValue({ configured: false });
    decisionMocks.getDecisionKeyStatus.mockReset().mockResolvedValue({ configured: false });
    decisionMocks.revealDecisionApiKey.mockReset().mockResolvedValue("stored-decision-key");
    decisionMocks.saveDecisionApiKey.mockReset().mockImplementation((_kind: string, secret: string) => Promise.resolve({
      configured: true,
      keyLength: secret.length
    }));
  });

  it("shows exactly one selected TypeSafe provider and only its API key field", async () => {
    const { container } = render(<DecisionProviderSettings />);

    expect(screen.getAllByRole("button", { name: "TypeSafe" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "TypeSafe" })).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("button", { name: "Add provider" })).toBeDisabled();
    expect(container.querySelectorAll("input")).toHaveLength(1);
    expect(screen.getByLabelText("API Key")).toHaveAttribute("type", "password");
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Endpoint")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Model")).not.toBeInTheDocument();
  });

  it("marks the rail row active from the stored key status", async () => {
    decisionMocks.getDecisionKeyStatus.mockResolvedValue({ configured: true, keyLength: 12 });
    const { container } = render(<DecisionProviderSettings />);

    await waitFor(() => expect(container.querySelector(".provider-rail__dot")).toBeInTheDocument());
    expect(screen.getByLabelText("API Key")).toHaveValue("•".repeat(12));
  });

  it("flushes before saving a typed key", async () => {
    const user = userEvent.setup();
    const onFlush = vi.fn(() => Promise.resolve());
    render(<DecisionProviderSettings onFlush={onFlush} />);
    const key = screen.getByLabelText("API Key");

    await user.type(key, "secret");
    await user.tab();

    await waitFor(() => expect(decisionMocks.saveDecisionApiKey).toHaveBeenCalledWith("typesafe", "secret"));
    expect(onFlush.mock.invocationCallOrder[0]).toBeLessThan(
      decisionMocks.saveDecisionApiKey.mock.invocationCallOrder[0]
    );
  });

  it("reveals the configured key only after the reveal button is pressed", async () => {
    const user = userEvent.setup();
    decisionMocks.getDecisionKeyStatus.mockResolvedValue({ configured: true, keyLength: 20 });
    render(<DecisionProviderSettings />);
    const key = screen.getByLabelText("API Key");

    await waitFor(() => expect(key).toHaveValue("•".repeat(20)));
    await user.click(screen.getByRole("button", { name: "Show API Key" }));

    await waitFor(() => expect(decisionMocks.revealDecisionApiKey).toHaveBeenCalledWith("typesafe"));
    expect(key).toHaveValue("stored-decision-key");
    expect(key).toHaveAttribute("type", "text");
  });

  it("deletes the key when the field is cleared and blurred", async () => {
    const user = userEvent.setup();
    decisionMocks.getDecisionKeyStatus.mockResolvedValue({ configured: true, keyLength: 6 });
    render(<DecisionProviderSettings />);
    const key = screen.getByLabelText("API Key");

    await waitFor(() => expect(key).toHaveValue("•".repeat(6)));
    await user.clear(key);
    await user.tab();

    await waitFor(() => expect(decisionMocks.deleteDecisionApiKey).toHaveBeenCalledWith("typesafe"));
  });
});
