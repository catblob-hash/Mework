import assert from "node:assert/strict";
import test from "node:test";

import { auditFrontendCsp } from "../check-frontend-csp.mjs";

const shared = {
  "default-src": "'none'",
  "script-src": "'self'",
  "script-src-attr": "'none'",
  "font-src": "'self'",
  "img-src": "'self' data:",
  "style-src": "'self'",
  "style-src-attr": "'none'",
  "object-src": "'none'",
  "worker-src": "'none'",
  "child-src": "'none'",

  "manifest-src": "'none'",
  "base-uri": "'none'",
  "form-action": "'none'",
  "frame-ancestors": "'none'"
};

function policies() {
  return {
    csp: {
      ...shared,
      "media-src": "'none'",
      "connect-src": "ipc: http://ipc.localhost",
      "frame-src": "'none'"
    },
    devCsp: {
      ...shared,
      "media-src": "'none'",
      "connect-src": "ipc: http://ipc.localhost http://localhost:1420 http://127.0.0.1:1420 ws://localhost:1420 ws://127.0.0.1:1420 ws://localhost:1430 ws://127.0.0.1:1430",
      "frame-src": "http: https:"
    }
  };
}

function baselineFiles() {
  return {
    "index.html": '<!doctype html><style id="mework-csp-style-nonce"></style><script type="module" src="/src/main.tsx"></script>',
    "memory-e2e.html": '<!doctype html><script type="module" src="/src/memoryE2E.tsx"></script>',
    "src/main.tsx": 'export const boot = () => document.createElement("main");',
    "src-tauri/tauri.conf.json": JSON.stringify({ app: { security: policies() } }),
    // Rust serves this as a remote-site fixture; its contents must not be
    // treated as the trusted renderer document.
    "src-tauri/resources/remote-fixture.html": "<style>body { color: red }</style><script>remoteFixture()</script>"
  };
}

test("accepts external-entry HTML, exact production/dev CSP, and Rust remote fixtures", () => {
  assert.deepEqual(auditFrontendCsp(baselineFiles()), []);
});

test("rejects inline HTML, remote entries, and dangerous frontend DOM sinks", () => {
  const files = baselineFiles();
  files["web-search-e2e.html"] = [
    '<main style="display: block" onclick="boot()">',
    "<style>main { display: block }</style>",
    "<script>boot()</script>",
    '<script src="https://cdn.example/app.js"></script>',
    '<link rel="stylesheet" href="//cdn.example/app.css">',
    "</main>"
  ].join("");
  files["src/unsafe.ts"] = [
    'element.cssText = "display:none";',
    'element.setAttribute("style", "display:none");',
    'element.setAttribute("onclick", "run()" );',
    'element.innerHTML = "<b>unsafe</b>";',
    'element.outerHTML = "<b>unsafe</b>";',
    'const props = { dangerouslySetInnerHTML: { __html: value } };',
    'const frame = <iframe srcDoc={value} />;',
    'element.insertAdjacentHTML("beforeend", "<b>unsafe</b>");',
    'document.write("<b>unsafe</b>");',
    'document.createElement("style");',
    'eval("unsafe");',
    'Function("return unsafe")();'
  ].join("\n");
  const codes = auditFrontendCsp(files).map(({ code }) => code);
  for (const expected of [
    "inline-style-attribute", "inline-event-attribute", "inline-style-tag",
    "inline-script-tag", "inline-script-content", "remote-script-src",
    "remote-stylesheet-src", "css-text", "style-attribute", "event-attribute",
    "inner-html", "outer-html", "dangerous-react-html", "src-doc",
    "insert-adjacent-html", "document-write", "style-element", "eval",
    "function-constructor"
  ]) assert.ok(codes.includes(expected), expected);
});

test("ignores comments, inert strings, test files, and template text without hiding template expressions", () => {
  const files = baselineFiles();
  files["src/safe.ts"] = [
    '// document.createElement("style");',
    'const note = "innerHTML and eval( are documentation";',
    'const template = `outerHTML ${document.createElement("main").localName}`;',
    'export { note, template };'
  ].join("\n");
  files["src/components/Example.test.tsx"] = 'document.body.innerHTML = "test-only fixture";';
  assert.deepEqual(auditFrontendCsp(files), []);

  files["src/safe.ts"] += '\nconst bad = `${document.createElement("style").nonce}`;';
  assert.ok(auditFrontendCsp(files).some(({ code }) => code === "style-element"));
});

test("rejects missing or malformed nonce carriers", () => {
  const missing = baselineFiles();
  missing["index.html"] = '<script type="module" src="/src/main.tsx"></script>';
  assert.ok(auditFrontendCsp(missing).some(({ code }) => code === "missing-style-nonce-carrier"));

  const nonEmpty = baselineFiles();
  nonEmpty["index.html"] = '<style id="mework-csp-style-nonce">body{}</style><script type="module" src="/src/main.tsx"></script>';
  const codes = auditFrontendCsp(nonEmpty).map(({ code }) => code);
  assert.ok(codes.includes("inline-style-tag"));
  assert.ok(codes.includes("missing-style-nonce-carrier"));
});

test("rejects incomplete, widened, duplicate, and unreviewed Tauri policy directives", () => {
  const files = baselineFiles();
  const strict = policies();
  const productionString = Object.entries(strict.csp)
    .map(([directive, sources]) => `${directive} ${sources}`)
    .join("; ");
  files["src-tauri/tauri.conf.json"] = JSON.stringify({
    app: {
      security: {
        csp: `script-src 'unsafe-inline'; ${productionString}`,
        devCsp: {
          ...strict.devCsp,
          "connect-src": "*",
          "report-uri": "https://collector.example/csp"
        }
      }
    }
  });
  const codes = auditFrontendCsp(files).map(({ code }) => code);
  assert.ok(codes.includes("duplicate-script-src"));
  assert.ok(codes.includes("unsafe-script-src"));
  assert.ok(codes.includes("sources-script-src"));
  assert.ok(codes.includes("sources-connect-src"));
  assert.ok(codes.includes("wildcard-connect-src"));
  assert.ok(codes.includes("unexpected-report-uri"));

  const incomplete = baselineFiles();
  incomplete["src-tauri/tauri.conf.json"] = JSON.stringify({
    app: { security: { csp: { "script-src-attr": "'none'" }, devCsp: strict.devCsp } }
  });
  assert.ok(auditFrontendCsp(incomplete).some(({ code }) => code === "missing-default-src"));
});
