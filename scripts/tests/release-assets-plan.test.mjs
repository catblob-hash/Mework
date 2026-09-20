import assert from "node:assert/strict";
import test from "node:test";

import {
  formatSha256Sums,
  parseSha256Sums,
  releaseAssetNames
} from "../release-assets-plan.mjs";

const SHA_A = "A".repeat(64);
const SHA_B = "b".repeat(64);

test("returns canonical release asset names", () => {
  assert.deepEqual(releaseAssetNames("1.2.3"), {
    installer: "Mework_1.2.3_x64-setup.exe",
    portable: "Mework_1.2.3_x64_portable.zip",
    checksums: "SHA256SUMS"
  });
});

test("formats sorted GNU sha256sum lines with lowercase hashes", () => {
  assert.equal(
    formatSha256Sums([
      { name: "zeta.zip", sha256: SHA_B },
      { name: "alpha.exe", sha256: SHA_A }
    ]),
    `${SHA_A.toLowerCase()}  alpha.exe\n${SHA_B}  zeta.zip\n`
  );
});

test("rejects invalid checksums and unsafe asset names", () => {
  assert.throws(
    () => formatSha256Sums([{ name: "asset.exe", sha256: "a".repeat(63) }]),
    /SHA-256/u
  );
  for (const name of ["", "has space.exe", "dir/asset.exe", "dir\\asset.exe"]) {
    assert.throws(
      () => formatSha256Sums([{ name, sha256: SHA_B }]),
      /asset name/u
    );
  }
});

test("parses GNU separators, CRLF, comments, and blank lines", () => {
  assert.deepEqual(
    parseSha256Sums([
      "# Release checksums",
      "",
      `${SHA_A}  installer.exe`,
      `${SHA_B} *portable.zip`,
      ""
    ].join("\r\n")),
    [
      { name: "installer.exe", sha256: SHA_A.toLowerCase() },
      { name: "portable.zip", sha256: SHA_B }
    ]
  );
});

test("rejects malformed SHA256SUMS lines", () => {
  for (const text of [
    `${SHA_B} portable.zip`,
    `${SHA_B}  name with spaces.zip`,
    `${"x".repeat(64)}  portable.zip`,
    "not a checksum"
  ]) {
    assert.throws(() => parseSha256Sums(text), /Malformed SHA256SUMS line/u);
  }
});

test("round-trips formatted checksum entries", () => {
  const entries = [
    { name: "Mework_1.2.3_x64_portable.zip", sha256: SHA_B },
    { name: "Mework_1.2.3_x64-setup.exe", sha256: SHA_A }
  ];
  assert.deepEqual(
    parseSha256Sums(formatSha256Sums(entries)),
    [
      { name: "Mework_1.2.3_x64-setup.exe", sha256: SHA_A.toLowerCase() },
      { name: "Mework_1.2.3_x64_portable.zip", sha256: SHA_B }
    ]
  );
});
