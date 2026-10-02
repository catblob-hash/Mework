import type { ApiProvider, AppDocument, ModelProfile } from "../types";
import { isClaudeAgentProvider } from "./claudeAgentProvider";
import { claudeAgentLoginStatus, fetchModels } from "./runtime";

/**
 * First launch with Claude Code signed in: the Claude Agent row takes the
 * newest model of each Claude line from the bundled CLI's own list, and the
 * composer starts on its Opus. Without a login, or when the list comes back
 * empty, the install keeps the seed rows it shipped with.
 */

/** The Claude model lines a first launch keeps one model of each, in this order. */
const CLAUDE_MODEL_LINES = ["fable", "opus", "sonnet", "haiku"] as const;

/**
 * `claude-<line>-<major>[-<minor>][-<YYYYMMDD>]`: the ids Claude Code's picker
 * resolves to (`claude-opus-5-5`, `claude-haiku-4-5-20251001`). The minor is one
 * or two digits so a bare dated id (`claude-opus-4-20250514`) reads as 4.0.
 * Aliases the CLI could not resolve (`opus`) carry no version and are skipped.
 */
const CLAUDE_MODEL_ID = /^claude-(fable|opus|sonnet|haiku)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?$/;

/**
 * The newest model of each line in `models`, in `CLAUDE_MODEL_LINES` order.
 * Among ids of one version (an alias and its dated snapshot) the first listed wins.
 */
export function newestClaudeModelPerLine(models: readonly ModelProfile[]): ModelProfile[] {
  const newest = new Map<string, { model: ModelProfile; major: number; minor: number }>();
  for (const model of models) {
    const match = CLAUDE_MODEL_ID.exec(model.id);
    if (!match) continue;
    const [, line, major, minor] = match;
    const version = { major: Number(major), minor: Number(minor ?? 0) };
    const current = newest.get(line);
    if (current && (current.major > version.major
      || (current.major === version.major && current.minor >= version.minor))) continue;
    newest.set(line, { model, ...version });
  }
  return CLAUDE_MODEL_LINES.flatMap((line) => newest.get(line)?.model ?? []);
}

/**
 * What a first launch must find unchanged before it may replace anything: the
 * Claude Agent row's models, its active model and switch, and the global choice.
 * The fetch takes seconds; a user who picked a model meanwhile keeps it.
 */
function claudeAgentSelectionKey(document: AppDocument): string {
  const row = document.globalSettings.apiProviders.find(isClaudeAgentProvider);
  return JSON.stringify([
    document.globalSettings.activeProviderId,
    row?.id ?? null,
    row?.enabled ?? null,
    row?.activeModelId ?? null,
    row?.models.map((model) => model.id) ?? null
  ]);
}

/**
 * `document` with the Claude Agent row holding the newest model of each line in
 * `fetched`, enabled, and selected in the composer on its Opus (else its first).
 * `null` when there is nothing to adopt or no row to adopt it into.
 */
export function adoptFirstLaunchClaudeModels(
  document: AppDocument,
  fetched: readonly ModelProfile[]
): AppDocument | null {
  const models = newestClaudeModelPerLine(fetched);
  const row = document.globalSettings.apiProviders.find(isClaudeAgentProvider);
  if (!models.length || !row) return null;
  const active = models.find((model) => CLAUDE_MODEL_ID.exec(model.id)?.[1] === "opus") ?? models[0];
  const adopted: ApiProvider = { ...row, enabled: true, models, activeModelId: active.id };
  return {
    ...document,
    globalSettings: {
      ...document.globalSettings,
      apiProviders: document.globalSettings.apiProviders.map((provider) => (
        provider.id === row.id ? adopted : provider
      )),
      activeProviderId: row.id
    }
  };
}

/**
 * The Claude Agent row's model list as the bundled CLI reports it, or `[]` when
 * the CLI is not signed in. Both questions start the CLI, so this takes seconds.
 */
async function fetchFirstLaunchClaudeModels(document: AppDocument): Promise<ModelProfile[]> {
  const row = document.globalSettings.apiProviders.find(isClaudeAgentProvider);
  if (!row) return [];
  const status = await claudeAgentLoginStatus(row);
  if (!status.signedIn) return [];
  return fetchModels(row);
}

let started = false;

/**
 * Runs the first-launch setup once per renderer: asks the CLI in the background
 * and, when it answers with models, adopts them through `update` — but only into
 * a document whose Claude selection is still what `loaded` had. A CLI that fails
 * or is signed out leaves everything as it was.
 */
export async function setUpClaudeAgentOnFirstLaunch(
  loaded: AppDocument,
  update: (updater: (current: AppDocument | null) => AppDocument | null) => void
): Promise<void> {
  if (started) return;
  started = true;
  const before = claudeAgentSelectionKey(loaded);
  let fetched: ModelProfile[];
  try {
    fetched = await fetchFirstLaunchClaudeModels(loaded);
  } catch {
    return;
  }
  update((current) => {
    if (!current || claudeAgentSelectionKey(current) !== before) return current;
    return adoptFirstLaunchClaudeModels(current, fetched) ?? current;
  });
}
