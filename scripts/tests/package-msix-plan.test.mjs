import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { msixFileNames, msixVersion, renderManifest, validateIdentity } from "../package-msix-plan.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const identity = {
  name: "12345Catblob.Mework",
  publisher: "CN=01234567-89AB-CDEF-0123-456789ABCDEF",
  publisherDisplayName: "catblob"
};

test("an MSIX version is the release version with a zero revision", () => {
  assert.equal(msixVersion("1.0.0"), "1.0.0.0");
  assert.equal(msixVersion(" 2.10.3 "), "2.10.3.0");
  assert.throws(() => msixVersion("1.0.0-beta.1"), /prerelease/u);
  assert.throws(() => msixVersion("1.0"), /major\.minor\.patch/u);
  assert.throws(() => msixVersion("70000.0.0"), /65535/u);
});

test("the identity must be what Partner Center assigned", () => {
  assert.deepEqual(validateIdentity(identity), identity);
  assert.throws(() => validateIdentity({ ...identity, name: "Me work" }), /identity\.name/u);
  assert.throws(() => validateIdentity({ ...identity, publisher: "catblob" }), /identity\.publisher/u);
  assert.throws(() => validateIdentity({ ...identity, publisherDisplayName: " " }), /publisherDisplayName/u);
});

test("the committed template renders every field and nothing else", () => {
  const template = fs.readFileSync(path.join(root, "src-tauri", "msix", "AppxManifest.xml"), "utf8");
  const manifest = renderManifest(template, {
    identity: { ...identity, publisherDisplayName: "Cat & Blob" },
    version: "1.0.0.0"
  });
  assert.doesNotMatch(manifest, /\{\{[A-Z_]+\}\}/u);
  assert.match(manifest, /Name="12345Catblob\.Mework"/u);
  assert.match(manifest, /Publisher="CN=01234567-89AB-CDEF-0123-456789ABCDEF"/u);
  assert.match(manifest, /Version="1\.0\.0\.0"/u);
  assert.match(manifest, /<PublisherDisplayName>Cat &amp; Blob<\/PublisherDisplayName>/u);
  // The executables the manifest and the runtime rely on.
  assert.match(manifest, /Executable="mework\.exe"/u);
  assert.match(manifest, /rescap:Capability Name="runFullTrust"/u);
  assert.throws(() => renderManifest("{{NOPE}}", { identity, version: "1.0.0.0" }), /unknown field/u);
});

test("every logo the manifest names has qualified variants", () => {
  const template = fs.readFileSync(path.join(root, "src-tauri", "msix", "AppxManifest.xml"), "utf8");
  const assets = fs.readdirSync(path.join(root, "src-tauri", "msix", "Assets"));
  const logos = [...template.matchAll(/Assets\\([A-Za-z0-9]+)\.png/gu)].map((match) => match[1]);
  assert.ok(logos.length >= 3, logos.join(", "));
  for (const logo of logos) {
    assert.ok(assets.includes(`${logo}.scale-100.png`), `${logo}.scale-100.png`);
    // makepri reads a bare name as scale-100 and refuses the pair.
    assert.ok(!assets.includes(`${logo}.png`), `${logo}.png must not exist`);
  }
});

test("the release names the updater must not mistake for its own", () => {
  const names = msixFileNames("1.0.0");
  assert.deepEqual(names, {
    signed: "Mework_1.0.0_x64.msix",
    store: "Mework_1.0.0_x64_store.msix",
    certificate: "Mework_msix_signing.cer"
  });
  for (const name of Object.values(names)) {
    assert.doesNotMatch(name, /[-_]setup\.exe$/iu);
    assert.ok(!(name.toLowerCase().endsWith(".zip") && name.toLowerCase().includes("portable")), name);
  }
});
