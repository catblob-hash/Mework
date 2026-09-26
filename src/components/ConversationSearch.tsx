import { Search } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { visibleConversations } from "../lib/draftConversation";
import { useFloatingSurface } from "../lib/floatingSurfaces";
import { isImeKeyEvent } from "../lib/shortcuts";
import { isTemporaryWorkspace } from "../lib/workspaces";
import type { Workspace } from "../types";

/** How many rows the palette lists; a longer query narrows it rather than scrolling on. */
const MAX_RESULTS = 60;

interface SearchEntry {
  workspaceId: string;
  conversationId: string;
  title: string;
  projectName: string;
  updatedAt: string;
  haystack: string;
}

interface ConversationSearchProps {
  workspaces: Workspace[];
  activeConversationId: string | null;
  onSelect: (workspaceId: string, conversationId: string) => void;
  onClose: () => void;
}

/**
 * Finds a conversation by title or project, across every project. An empty query lists the
 * most recently updated first, so the palette doubles as a jump list; every word of a query
 * has to appear, in any order, in the title or the project's name.
 */
export function ConversationSearch({ workspaces, activeConversationId, onSelect, onClose }: ConversationSearchProps) {
  const { t } = useI18n();
  const listId = useId();
  const backdropRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);

  // A modal over the whole window, like `Dialog`: the built-in browser's native page has to
  // step aside rather than paint over it.
  useFloatingSurface(backdropRef, true);

  const entries = useMemo<SearchEntry[]>(() => workspaces.flatMap((workspace) => {
    const projectName = isTemporaryWorkspace(workspace) ? t("临时项目", "Temporary project") : workspace.name;
    return visibleConversations(workspace.conversations).map((conversation) => ({
      workspaceId: workspace.id,
      conversationId: conversation.id,
      title: conversation.title,
      projectName,
      updatedAt: conversation.updatedAt,
      haystack: `${conversation.title}\n${projectName}`.toLocaleLowerCase()
    }));
  }), [t, workspaces]);

  const results = useMemo(() => {
    const terms = query.toLocaleLowerCase().split(/\s+/u).filter(Boolean);
    const matched = terms.length
      ? entries.filter((entry) => terms.every((term) => entry.haystack.includes(term)))
      : [...entries].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
    return matched.slice(0, MAX_RESULTS);
  }, [entries, query]);

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => previous?.focus?.();
  }, []);

  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-search-index="${cursor}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [cursor]);

  const choose = (entry: SearchEntry | undefined) => {
    if (!entry) return;
    onSelect(entry.workspaceId, entry.conversationId);
    onClose();
  };

  const optionId = (index: number) => `${listId}-option-${index}`;
  const current = Math.min(cursor, Math.max(0, results.length - 1));

  return createPortal(
    <div
      ref={backdropRef}
      className="conversation-search-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="conversation-search" role="dialog" aria-modal="true" aria-label={t("搜索对话", "Search conversations")}>
        <div className="conversation-search__field">
          <Search size={15} aria-hidden="true" />
          <input
            ref={inputRef}
            className="conversation-search__input"
            type="text"
            role="combobox"
            aria-expanded={results.length > 0}
            aria-controls={listId}
            aria-autocomplete="list"
            aria-activedescendant={results.length ? optionId(current) : undefined}
            aria-label={t("搜索对话", "Search conversations")}
            placeholder={t("按标题或项目搜索对话…", "Search conversations by title or project…")}
            spellCheck={false}
            autoComplete="off"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              // Every new query starts from its best match.
              setCursor(0);
            }}
            onKeyDown={(event) => {
              if (isImeKeyEvent(event.nativeEvent)) return;
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                onClose();
              } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                if (!results.length) return;
                const step = event.key === "ArrowDown" ? 1 : -1;
                setCursor((index) => (Math.min(index, results.length - 1) + step + results.length) % results.length);
              } else if (event.key === "Enter") {
                event.preventDefault();
                choose(results[current]);
              }
            }}
          />
        </div>
        <div ref={listRef} id={listId} className="conversation-search__results" role="listbox" aria-label={t("对话", "Conversations")}>
          {results.map((entry, index) => (
            <div
              key={`${entry.workspaceId}:${entry.conversationId}`}
              id={optionId(index)}
              role="option"
              aria-selected={index === current}
              tabIndex={-1}
              data-search-index={index}
              className={`conversation-search__result${index === current ? " conversation-search__result--current" : ""}${entry.conversationId === activeConversationId ? " conversation-search__result--open" : ""}`}
              onMouseMove={() => { if (index !== current) setCursor(index); }}
              // Keep the focus in the field, so typing carries on after a click that misses.
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(entry)}
            >
              <span className="conversation-search__title">{entry.title}</span>
              <span className="conversation-search__project">{entry.projectName}</span>
            </div>
          ))}
        </div>
        {results.length === 0 && (
          <p className="conversation-search__empty">
            {entries.length ? t("没有匹配的对话", "No matching conversations") : t("还没有对话", "No conversations yet")}
          </p>
        )}
      </div>
    </div>,
    document.body
  );
}
