import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const self = path.resolve(fileURLToPath(import.meta.url));
const deadBrowserSurface = ["browser", "tools", "e2e"].join("-");
const deadBrowserModule = ["browser", "Tools", "E2E"].join("");
const deadExtensionSurface = ["web", "research", "extension"].join("-");
const deadSearchPage = ["web-search-e2e", "search.html"].join("-");
const deadSourcePage = ["web-search-e2e", "source.html"].join("-");
const deadCanvasOutput = ["canvas", "e2e", "output"].join("-");
const deadMultimediaSurface = ["multi", "media"].join("");
const deadMultimediaPage = [deadMultimediaSurface, "e2e"].join("-");
const deadMultimediaModule = [deadMultimediaSurface, "E2E"].join("");
// The generation module's own Rust files. Their stem is a word too common to
// put in `forbiddenTokens` (`@media`, `media_type`, "immediately"), so they are
// asserted absent by path only.
const retiredMediaStem = ["me", "dia"].join("");

const deadPaths = [
  `${deadBrowserSurface}.html`,
  path.join("src", `${deadBrowserModule}.ts`),
  path.join("src-tauri", "resources", `${deadBrowserSurface}.html`),
  path.join("src-tauri", "resources", deadSearchPage),
  path.join("src-tauri", "resources", deadSourcePage),
  path.join("src-tauri", "resources", deadExtensionSurface),
  path.join("scripts", "tests", `${deadExtensionSurface}-policy.test.mjs`),
  `${deadMultimediaPage}.html`,
  path.join("src", `${deadMultimediaModule}.ts`),
  path.join("scripts", `${deadMultimediaPage}.mjs`),
  path.join("src", "components", "ArtifactStrip.tsx"),
  path.join("docs", `${deadMultimediaSurface}.md`),
  path.join("src-tauri", "src", `${retiredMediaStem}_store.rs`),
  path.join("src-tauri", "src", `${retiredMediaStem}_playback.rs`),
  path.join("src-tauri", "src", "protocol_adapter", retiredMediaStem)
];

const sourceRoots = [
  "src",
  "scripts",
  path.join("src-tauri", "src"),
  path.join("src-tauri", "resources")
];

function collectFiles(directory, files = []) {
  if (!fs.existsSync(directory)) return files;
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      if ([
        "node_modules",
        "target",
        "dist",
        ".git",
        ".codex-tmp"
      ].includes(entry.name)) continue;
      collectFiles(target, files);
    } else if (path.resolve(target) !== self) {
      files.push(target);
    }
  }
  return files;
}

test("retired E2E fixture surfaces stay absent and unreferenced", () => {
  for (const relativePath of deadPaths) {
    assert.equal(fs.existsSync(path.join(root, relativePath)), false, `${relativePath} must stay retired`);
  }

  const files = [
    path.join(root, "package.json"),
    path.join(root, "vite.config.ts"),
    path.join(root, ".gitignore"),
    ...sourceRoots.flatMap((relativePath) => collectFiles(path.join(root, relativePath)))
  ];
  const forbiddenTokens = [
    deadBrowserSurface,
    deadBrowserModule,
    deadExtensionSurface,
    deadSearchPage,
    deadSourcePage,
    deadCanvasOutput,
    deadMultimediaSurface
  ];
  for (const file of files) {
    const source = fs.readFileSync(file, "utf8");
    for (const token of forbiddenTokens) {
      assert.equal(source.includes(token), false, `${path.relative(root, file)} still references ${token}`);
    }
  }
});

test("the three live E2E entry pages and native image fixture remain wired", () => {
  for (const entry of ["memory-e2e.html", "image-input-e2e.html", "web-search-e2e.html"]) {
    assert.equal(fs.existsSync(path.join(root, entry)), true, `${entry} must remain live`);
  }
  const packageSource = fs.readFileSync(path.join(root, "package.json"), "utf8");
  for (const script of ["test:memory-e2e", "test:image-input-e2e", "test:web-search-e2e"]) {
    assert.match(packageSource, new RegExp(`"${script}"`, "u"));
  }

  const nativeFixture = path.join("src-tauri", "resources", "image-input-browser-e2e.html");
  assert.equal(fs.existsSync(path.join(root, nativeFixture)), true, `${nativeFixture} must exist`);
  const browserDev = fs.readFileSync(path.join(root, "src-tauri", "src", "browser_dev.rs"), "utf8");
  assert.match(browserDev, /include_str!\("\.\.\/resources\/image-input-browser-e2e\.html"\)/u);
  assert.match(browserDev, /\.route\("\/image-input-browser-e2e",\s*get\(image_input_browser_e2e\)\)/u);
});
