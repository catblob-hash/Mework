import assert from "node:assert/strict";
import test from "node:test";

import {
  formatSha256Sums,
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
