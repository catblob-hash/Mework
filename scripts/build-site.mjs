// Builds the static documentation site under website/dist from the Markdown
// sources in website/content/<lang>/*.md.
//
// Usage: node scripts/build-site.mjs [--out <dir>] [--base <url-path>]
//
// The build is deliberately dependency-light: `marked` for Markdown, no
// framework. Each page is one Markdown file; the first `# ` heading is its
// title. Two placeholders are expanded from the generated design baselines in
// docs/context-injections/: `{{PROMPT_KEYS_TABLE}}` (the injection-point
// reference) and `{{PROFILE_FILE_LINKS}}` (download links for the two built-in
// profile files, which are copied into the site).

import { marked } from "marked";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "..");
const contentRoot = path.join(repo, "website", "content");
const assetsRoot = path.join(repo, "website", "assets");
const baselineRoot = path.join(repo, "docs", "context-injections");

const args = process.argv.slice(2);
function option(name, fallback) {
  const index = args.indexOf(name);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
}
const outRoot = path.resolve(repo, option("--out", "website/dist"));
// GitHub Pages serves a project site under /<repo>/; links are relative so the
// base only matters for the <base> tag and the language redirect.
const basePath = option("--base", "/");

const LANGUAGES = [
  { code: "en", label: "English", htmlLang: "en", dir: "en" },
  { code: "zh-CN", label: "简体中文", htmlLang: "zh-CN", dir: "zh-CN" },
];
const PAGES = ["index", "working", "prompt-profiles", "skills", "mcp", "hooks"];
// Short navigation labels; page titles (first heading) stay descriptive.
const NAV_LABELS = {
  en: { index: "Overview", working: "Working with Mework", "prompt-profiles": "Prompt profiles", skills: "Skills", mcp: "MCP", hooks: "Hooks" },
  "zh-CN": { index: "概览", working: "使用方法", "prompt-profiles": "提示词档案", skills: "技能", mcp: "MCP", hooks: "钩子" },
};
const UI = {
  en: {
    siteName: "Mework docs",
    tagline: "Mework — Mew. Work.",
    github: "GitHub",
    onThisPage: "On this page",
    languages: "Language",
    footer: "Mework is released under GPL-3.0-or-later. This site is built from website/content in the repository.",
    keyTable: { id: "Key", placeholders: "Placeholders", where: "Where it is injected", english: "Built-in English text", chinese: "Built-in Chinese text" },
    downloads: { english: "Download the built-in English profile (prompt-profile.en-US.json)", chinese: "Download the built-in Chinese profile (prompt-profile.zh-CN.json)", manifest: "Download the key manifest (prompt-profile-keys.json)" },
  },
  "zh-CN": {
    siteName: "Mework 文档",
    tagline: "Mework — Mew. Work.",
    github: "GitHub",
    onThisPage: "本页目录",
    languages: "语言",
    footer: "Mework 以 GPL-3.0-or-later 许可发布。本站由仓库中的 website/content 构建。",
    keyTable: { id: "键", placeholders: "占位符", where: "注入位置", english: "内置英文文本", chinese: "内置中文文本" },
    downloads: { english: "下载内置英文文件（prompt-profile.en-US.json）", chinese: "下载内置中文文件（prompt-profile.zh-CN.json）", manifest: "下载键清单（prompt-profile-keys.json）" },
  },
};

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function escapeHtml(text) {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function slugify(text) {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, "")
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "") || "section";
}

/** Renders the injection-point reference table from the generated baselines. */
function promptKeysTable(lang) {
  const manifest = readJson(path.join(baselineRoot, "prompt-profile-keys.json"));
  const english = readJson(path.join(baselineRoot, "prompt-profile.en-US.json")).prompts;
  const chinese = readJson(path.join(baselineRoot, "prompt-profile.zh-CN.json")).prompts;
  const labels = UI[lang].keyTable;
  const rows = manifest.keys.map((key) => {
    const placeholders = key.placeholders.length
      ? key.placeholders.map((name) => `<code>{${escapeHtml(name)}}</code>`).join(" ")
      : "—";
    return `<tr id="key-${escapeHtml(key.id).replace(/\./g, "-")}">
  <td><code>${escapeHtml(key.id)}</code></td>
  <td>${placeholders}</td>
  <td>${escapeHtml(key.description)}</td>
  <td><pre class="key-text">${escapeHtml(english[key.id] ?? "")}</pre></td>
  <td><pre class="key-text">${escapeHtml(chinese[key.id] ?? "")}</pre></td>
</tr>`;
  });
  return `<div class="table-scroll"><table class="key-table">
<thead><tr><th>${labels.id}</th><th>${labels.placeholders}</th><th>${labels.where}</th><th>${labels.english}</th><th>${labels.chinese}</th></tr></thead>
<tbody>
${rows.join("\n")}
</tbody></table></div>`;
}

function profileFileLinks(lang) {
  const labels = UI[lang].downloads;
  return `<ul class="downloads">
<li><a href="../files/prompt-profile.en-US.json" download>${labels.english}</a></li>
<li><a href="../files/prompt-profile.zh-CN.json" download>${labels.chinese}</a></li>
<li><a href="../files/prompt-profile-keys.json" download>${labels.manifest}</a></li>
</ul>`;
}

/** Markdown → HTML with heading ids and a collected outline. */
function renderMarkdown(markdown, lang) {
  const outline = [];
  const renderer = new marked.Renderer();
  // A heading may end in `{#custom-id}` so that anchors survive translation:
  // the id is shared across languages while the visible text is not.
  renderer.heading = ({ tokens, depth }) => {
    let text = renderer.parser.parseInline(tokens);
    let id = slugify(text);
    const explicit = /\s*\{#([A-Za-z0-9_-]+)\}\s*$/.exec(text);
    if (explicit) {
      id = explicit[1];
      text = text.slice(0, explicit.index);
    }
    if (depth === 2) outline.push({ id, text });
    return `<h${depth} id="${id}"><a class="anchor" href="#${id}">${text}</a></h${depth}>\n`;
  };
  const expanded = markdown
    .replace(/\{\{PROMPT_KEYS_TABLE\}\}/g, () => promptKeysTable(lang))
    .replace(/\{\{PROFILE_FILE_LINKS\}\}/g, () => profileFileLinks(lang));
  const html = marked.parse(expanded, { renderer, gfm: true });
  return { html, outline };
}

function pageTitle(markdown, fallback) {
  const match = /^#\s+(.+)$/m.exec(markdown);
  return match ? match[1].replace(/\s*\{#[A-Za-z0-9_-]+\}\s*$/, "").trim() : fallback;
}

function layout({ lang, page, title, html, outline, titles }) {
  const ui = UI[lang];
  const other = LANGUAGES.filter((candidate) => candidate.code !== lang);
  const nav = PAGES.map((candidate) =>
    `<a href="${candidate}.html"${candidate === page ? ' class="active"' : ""}>${escapeHtml(NAV_LABELS[lang][candidate] ?? titles[candidate])}</a>`
  ).join("\n        ");
  const languageLinks = LANGUAGES.map((candidate) =>
    candidate.code === lang
      ? `<span class="active">${candidate.label}</span>`
      : `<a href="../${candidate.dir}/${page}.html" hreflang="${candidate.htmlLang}">${candidate.label}</a>`
  ).join(" · ");
  const toc = outline.length
    ? `<aside class="toc"><h3>${ui.onThisPage}</h3><ul>${outline.map((item) => `<li><a href="#${item.id}">${item.text}</a></li>`).join("")}</ul></aside>`
    : "";
  void other;
  return `<!DOCTYPE html>
<html lang="${LANGUAGES.find((candidate) => candidate.code === lang).htmlLang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)} · ${ui.siteName}</title>
<link rel="stylesheet" href="../assets/style.css">
<link rel="icon" href="../assets/mework-icon.svg" type="image/svg+xml">
</head>
<body>
<header class="site-header">
  <div class="site-header__inner">
    <a class="brand" href="index.html"><img src="../assets/mework-icon.svg" alt="" width="28" height="28"> <span>${ui.siteName}</span></a>
    <nav class="site-nav">
        ${nav}
    </nav>
    <div class="site-header__right">
      <span class="languages" aria-label="${ui.languages}">${languageLinks}</span>
      <a class="github" href="https://github.com/catblob-hash/Mework">${ui.github}</a>
    </div>
  </div>
</header>
<div class="page">
  <main class="content">
${html}
  </main>
  ${toc}
</div>
<footer class="site-footer">${ui.footer}</footer>
</body>
</html>
`;
}

function copyDir(from, to) {
  fs.mkdirSync(to, { recursive: true });
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    const source = path.join(from, entry.name);
    const target = path.join(to, entry.name);
    if (entry.isDirectory()) copyDir(source, target);
    else fs.copyFileSync(source, target);
  }
}

function build() {
  fs.rmSync(outRoot, { recursive: true, force: true });
  fs.mkdirSync(outRoot, { recursive: true });
  copyDir(assetsRoot, path.join(outRoot, "assets"));
  fs.copyFileSync(path.join(repo, "src", "mework-icon.svg"), path.join(outRoot, "assets", "mework-icon.svg"));
  const filesDir = path.join(outRoot, "files");
  fs.mkdirSync(filesDir, { recursive: true });
  for (const name of ["prompt-profile.en-US.json", "prompt-profile.zh-CN.json", "prompt-profile-keys.json"]) {
    fs.copyFileSync(path.join(baselineRoot, name), path.join(filesDir, name));
  }
  fs.writeFileSync(path.join(outRoot, ".nojekyll"), "");

  for (const language of LANGUAGES) {
    const sourceDir = path.join(contentRoot, language.dir);
    const sources = Object.fromEntries(PAGES.map((page) => {
      const file = path.join(sourceDir, `${page}.md`);
      if (!fs.existsSync(file)) throw new Error(`missing page: ${path.relative(repo, file)}`);
      return [page, fs.readFileSync(file, "utf8")];
    }));
    const titles = Object.fromEntries(PAGES.map((page) => [page, pageTitle(sources[page], page)]));
    const targetDir = path.join(outRoot, language.dir);
    fs.mkdirSync(targetDir, { recursive: true });
    for (const page of PAGES) {
      const { html, outline } = renderMarkdown(sources[page], language.code);
      fs.writeFileSync(
        path.join(targetDir, `${page}.html`),
        layout({ lang: language.code, page, title: titles[page], html, outline, titles })
      );
    }
  }

  // The root page picks the reader's language once, then remembers nothing:
  // every page carries explicit language links.
  fs.writeFileSync(path.join(outRoot, "index.html"), `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mework docs</title>
<link rel="stylesheet" href="assets/style.css">
<script>
(function () {
  var zh = /^zh/i.test(navigator.language || "");
  location.replace((zh ? "zh-CN" : "en") + "/index.html");
})();
</script>
</head>
<body>
<main class="content landing">
<h1>Mework docs</h1>
<p><a href="en/index.html">English</a> · <a href="zh-CN/index.html">简体中文</a></p>
</main>
</body>
</html>
`);
  console.log(`built ${LANGUAGES.length * PAGES.length} pages into ${path.relative(repo, outRoot)}`);
}

build();
