import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";

import {
  MIRROR,
  buildManifest,
  canonicalPath,
  credentialsFromApiToken,
  expectedDigests,
  githubDownloadUrl,
  mirrorUrl,
  objectHeaders,
  objectKey,
  parseSha256Sums,
  signS3Request
} from "../release-mirror-plan.mjs";

const SHA_A = "a".repeat(64);
const SHA_B = "b".repeat(64);
const SHA_C = "c".repeat(64);

test("mirror keys follow GitHub's tag and file name", () => {
  assert.equal(objectKey("v1.0.0", "Mework_1.0.0_aarch64.dmg"), "v1.0.0/Mework_1.0.0_aarch64.dmg");
  assert.equal(mirrorUrl("v1.2.3-beta.1", "SHA256SUMS"), "https://dl.mework.dev/v1.2.3-beta.1/SHA256SUMS");
  assert.equal(
    githubDownloadUrl("v1.0.0", "Mework_1.0.0_x64-setup.exe"),
    "https://github.com/catblob-hash/Mework/releases/download/v1.0.0/Mework_1.0.0_x64-setup.exe"
  );
  for (const tag of ["1.0.0", "v1.0", "nightly", "v1.0.0/..", "../v1.0.0"]) {
    assert.throws(() => objectKey(tag, "a.exe"), /release tag/u, tag);
  }
  for (const name of ["", ".hidden", "dir/a.exe", "a b.exe", "..", "a\\b.exe", "é.exe", "x".repeat(129)]) {
    assert.throws(() => objectKey("v1.0.0", name), /asset name/u, name);
  }
});

test("files download as attachments with their own type; the checksum list reads inline", () => {
  assert.deepEqual(objectHeaders("Mework_1.0.0_aarch64.dmg"), {
    "cache-control": "public, max-age=86400, s-maxage=2592000",
    "content-type": "application/x-apple-diskimage",
    "content-disposition": "attachment; filename=\"Mework_1.0.0_aarch64.dmg\""
  });
  assert.equal(objectHeaders("Mework_1.0.0_x64.msix")["content-type"], "application/msix");
  assert.equal(objectHeaders("Mework_msix_signing.cer")["content-type"], "application/pkix-cert");
  assert.equal(objectHeaders("unknown.bin")["content-type"], "application/octet-stream");
  const sums = objectHeaders("SHA256SUMS");
  assert.equal(sums["content-type"], "text/plain; charset=utf-8");
  assert.equal(sums["content-disposition"], undefined);
});

test("SHA256SUMS parses like the updater's parser", () => {
  const sums = parseSha256Sums(`# list\r\n${SHA_A.toUpperCase()}  one.exe\r\n\n${SHA_B} *two.zip\n`);
  assert.deepEqual([...sums], [["one.exe", SHA_A], ["two.zip", SHA_B]]);
  assert.throws(() => parseSha256Sums("abc  short.exe"), /line 1/u);
  assert.throws(() => parseSha256Sums(SHA_A), /line 1/u);
});

test("every asset needs a SHA-256, and GitHub and SHA256SUMS must agree", () => {
  const assets = [
    { name: "one.exe", digest: `sha256:${SHA_A}` },
    { name: "two.zip", digest: null },
    { name: "SHA256SUMS", digest: `sha256:${SHA_C}` }
  ];
  const sums = `${SHA_A}  one.exe\n${SHA_B}  two.zip\n`;
  assert.deepEqual([...expectedDigests(assets, sums)], [["one.exe", SHA_A], ["two.zip", SHA_B], ["SHA256SUMS", SHA_C]]);
  // GitHub's digest alone is enough.
  assert.deepEqual([...expectedDigests([{ name: "one.exe", digest: `sha256:${SHA_A.toUpperCase()}` }], null)], [["one.exe", SHA_A]]);

  assert.throws(() => expectedDigests([{ name: "one.exe", digest: `sha256:${SHA_B}` }], sums.split("\n")[0]), /GitHub records/u);
  assert.throws(() => expectedDigests([{ name: "two.zip" }], null), /neither/u);
  assert.throws(() => expectedDigests([{ name: "one.exe", digest: `sha256:${SHA_A}` }], sums), /does not have: two\.zip/u);
  assert.throws(() => expectedDigests([{ name: "../one.exe", digest: `sha256:${SHA_A}` }], null), /asset name/u);
});

test("latest.json has the shape of GitHub's release object, pointing at the mirror", () => {
  const release = {
    tag_name: "v1.0.0",
    name: "Mework 1.0.0",
    html_url: "https://github.com/catblob-hash/Mework/releases/tag/v1.0.0",
    published_at: "2026-10-03T04:00:00Z",
    prerelease: false,
    body: "## Notes",
    assets: [{ name: "Mework_1.0.0_aarch64.dmg", size: 257442943, digest: `sha256:${SHA_A}` }]
  };
  const manifest = buildManifest(release, new Map([["Mework_1.0.0_aarch64.dmg", SHA_A]]), "2026-10-03T05:00:00.000Z");
  assert.deepEqual(manifest, {
    tag_name: "v1.0.0",
    name: "Mework 1.0.0",
    html_url: "https://github.com/catblob-hash/Mework/releases/tag/v1.0.0",
    published_at: "2026-10-03T04:00:00Z",
    draft: false,
    prerelease: false,
    body: "## Notes",
    assets: [{
      name: "Mework_1.0.0_aarch64.dmg",
      size: 257442943,
      digest: `sha256:${SHA_A}`,
      content_type: "application/x-apple-diskimage",
      browser_download_url: "https://dl.mework.dev/v1.0.0/Mework_1.0.0_aarch64.dmg",
      github_download_url: "https://github.com/catblob-hash/Mework/releases/download/v1.0.0/Mework_1.0.0_aarch64.dmg"
    }],
    mirrored_at: "2026-10-03T05:00:00.000Z"
  });
  assert.throws(() => buildManifest(release, new Map(), ""), /no verified SHA-256/u);
  assert.equal(MIRROR.manifestKey, "latest.json");
});

test("S3 credentials come from the API token's id and the SHA-256 of its value", () => {
  assert.deepEqual(credentialsFromApiToken("id123", "secret-value"), {
    accessKeyId: "id123",
    secretAccessKey: createHash("sha256").update("secret-value").digest("hex")
  });
  assert.throws(() => credentialsFromApiToken("", "x"), /required/u);
});

test("SigV4 reproduces AWS's published GET Object example", () => {
  // docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html, "Example: GET Object".
  const headers = signS3Request({
    method: "GET",
    url: "https://examplebucket.s3.amazonaws.com/test.txt",
    headers: { Range: "bytes=0-9" },
    payloadSha256: createHash("sha256").update("").digest("hex"),
    credentials: { accessKeyId: "AKIAIOSFODNN7EXAMPLE", secretAccessKey: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" },
    region: "us-east-1",
    now: new Date("2013-05-24T00:00:00Z")
  });
  assert.equal(
    headers.authorization,
    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, "
      + "SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, "
      + "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
  );
  assert.equal(headers["x-amz-date"], "20130524T000000Z");
  assert.equal(headers.host, undefined, "fetch sets the host itself");
});

test("SigV4 signs the query in sorted, encoded form", () => {
  // docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html, "Example: GET Bucket Lifecycle".
  const headers = signS3Request({
    method: "GET",
    url: "https://examplebucket.s3.amazonaws.com/?lifecycle=",
    payloadSha256: createHash("sha256").update("").digest("hex"),
    credentials: { accessKeyId: "AKIAIOSFODNN7EXAMPLE", secretAccessKey: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" },
    region: "us-east-1",
    now: new Date("2013-05-24T00:00:00Z")
  });
  assert.match(headers.authorization, /Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543$/u);
  assert.equal(canonicalPath("mework-releases", "v1.0.0/Mework_1.0.0_aarch64.dmg"), "/mework-releases/v1.0.0/Mework_1.0.0_aarch64.dmg");
  assert.equal(canonicalPath("b", "a b(1).txt"), "/b/a%20b%281%29.txt");
});
