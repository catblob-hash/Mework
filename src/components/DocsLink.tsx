import { BookOpen, ExternalLink } from "lucide-react";
import { useI18n } from "../i18n";

/**
 * A one-line link to the published configuration documentation.
 *
 * The pages that carry it — features, skills, MCP, hooks — used to end their
 * toolbar with bulk enable/disable buttons. Those acted on everything the search
 * happened to be showing, which is not a decision a user makes about a tool
 * surface; what they actually need there is the page explaining what these
 * entries are and where they are configured. It is deliberately a bare `<a>`
 * with no wrapper: it sits in whatever row it is dropped into.
 */

const DOCUMENTATION_ORIGIN = "https://catblob-hash.github.io/Mework";

/** The site page that documents each configuration surface. */
const DOCUMENTATION_PAGES = {
  features: "working",
  skills: "skills",
  mcp: "mcp",
  hooks: "hooks"
} as const;

function documentationLocale(resolvedLanguage: string): string {
  return resolvedLanguage === "zh-CN" ? "zh-CN" : "en";
}

export function DocsLink({ page }: { page: keyof typeof DOCUMENTATION_PAGES }) {
  const { t, resolvedLanguage } = useI18n();
  const locale = documentationLocale(resolvedLanguage);
  return (
    <a
      className="docs-link"
      href={`${DOCUMENTATION_ORIGIN}/${locale}/${DOCUMENTATION_PAGES[page]}.html`}
      target="_blank"
      rel="noreferrer noopener"
    >
      <ExternalLink size={11} aria-hidden="true" />
      {t("配置说明文档", "Configuration docs")}
    </a>
  );
}

/**
 * The way in to one tool's documentation, in the slot a group heading puts its
 * icon.
 *
 * It is an anchor and not a button because it leaves the application, and it is
 * a SIBLING of the tool's own toggle rather than a child of it: the row is
 * itself a button, and a link inside a button is neither valid nor operable.
 */
function ToolDocsAnchor({ path, label }: { path: string; label: string }) {
  const { t, resolvedLanguage } = useI18n();
  const locale = documentationLocale(resolvedLanguage);
  return (
    <a
      className="tool-docs-link"
      href={`${DOCUMENTATION_ORIGIN}/${locale}/${path}`}
      target="_blank"
      rel="noreferrer noopener"
      aria-label={t("{label}的说明文档", "Documentation for {label}", { label })}
      title={t("{label}的说明文档", "Documentation for {label}", { label })}
    >
      <BookOpen size={14} aria-hidden="true" />
    </a>
  );
}

/**
 * One tool's own page. The site publishes one per catalog tool at
 * `<locale>/tools/<name>.html` (built by scripts/build-site.mjs from the
 * catalog, so every tool name in the picker has a page).
 */
export function ToolDocsLink({ name, label }: { name: string; label: string }) {
  return <ToolDocsAnchor path={`tools/${encodeURIComponent(name)}.html`} label={label} />;
}
