// What the release mirror at dl.mework.dev holds and how it is written. Pure
// functions only; scripts/release-mirror.mjs does the network work.
//
// Layout of the R2 bucket behind https://dl.mework.dev:
//   <tag>/<asset name>   every file of a GitHub release, byte for byte, under
//                        the name GitHub gives it (v1.0.0/Mework_1.0.0_aarch64.dmg)
//   latest.json          the latest release in the shape of GitHub's release
//                        object, its download URLs pointing at the mirror;
//                        written only after every file it lists is in place
//
// A file is mirrored only when its SHA-256 matches both what GitHub recorded
// for the asset (`digest`) and the release's SHA256SUMS line, whichever exist,
// and the upload signs that same digest, so R2 refuses any other bytes.

import { createHash, createHmac } from "node:crypto";

export const MIRROR = {
  origin: "https://dl.mework.dev",
  bucket: "mework-releases",
  manifestKey: "latest.json",
  repository: "catblob-hash/Mework",
};

const TAG_PATTERN = /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;
const NAME_PATTERN = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,127}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;

/** Throws unless `name` is a plain file name: no separators, no leading dot, ASCII only. */
export function assertAssetName(name) {
  if (typeof name !== "string" || !NAME_PATTERN.test(name)) throw new TypeError(`not a plain asset name: ${name}`);
  return name;
}

/** The bucket key of one release file. Throws for anything that is not a plain tag and file name. */
export function objectKey(tag, name) {
  if (typeof tag !== "string" || !TAG_PATTERN.test(tag)) throw new TypeError(`not a release tag: ${tag}`);
  return `${tag}/${assertAssetName(name)}`;
}

/** The public address of one release file. */
export function mirrorUrl(tag, name) {
  return `${MIRROR.origin}/${objectKey(tag, name)}`;
}

/** The address GitHub serves the same file from. */
export function githubDownloadUrl(tag, name, repository = MIRROR.repository) {
  return `https://github.com/${repository}/releases/download/${objectKey(tag, name)}`;
}

const CONTENT_TYPES = [
  [/\.dmg$/i, "application/x-apple-diskimage"],
  [/\.exe$/i, "application/vnd.microsoft.portable-executable"],
  [/\.zip$/i, "application/zip"],
  [/\.msix$/i, "application/msix"],
  [/\.cer$/i, "application/pkix-cert"],
  [/^SHA256SUMS(?:\.txt)?$/i, "text/plain; charset=utf-8"],
  [/\.json$/i, "application/json; charset=utf-8"],
];

/**
 * The headers a mirrored file is stored with. A release file keeps its bytes
 * for good unless the release is corrected by hand, and the mirror purges the
 * edge copy when that happens, so the CDN may keep it for a month; a browser
 * re-asks after a day. Everything but the checksum list downloads as a file.
 */
export function objectHeaders(name) {
  const type = CONTENT_TYPES.find(([pattern]) => pattern.test(name))?.[1] ?? "application/octet-stream";
  const headers = {
    "cache-control": "public, max-age=86400, s-maxage=2592000",
    "content-type": type,
  };
  if (!type.startsWith("text/")) headers["content-disposition"] = `attachment; filename="${name}"`;
  return headers;
}

/** latest.json changes with every release, so nothing keeps it for more than a minute. */
export const MANIFEST_HEADERS = {
  "cache-control": "public, max-age=60",
  "content-type": "application/json; charset=utf-8",
};

/** GNU `sha256sum` output as a name → lowercase hex map. Same rules as the in-app updater's parser. */
export function parseSha256Sums(text) {
  const entries = new Map();
  text.split("\n").forEach((rawLine, index) => {
    const line = rawLine.replace(/\r$/, "").trim();
    if (!line || line.startsWith("#")) return;
    const match = /^([0-9A-Fa-f]{64})\s+\*?(\S.*)$/.exec(line);
    if (!match) throw new Error(`SHA256SUMS line ${index + 1} is not "<sha256>  <name>"`);
    entries.set(match[2].trim(), match[1].toLowerCase());
  });
  return entries;
}

/** `sha256:<hex>` as GitHub reports an asset's digest, or null. */
export function githubDigest(asset) {
  const match = /^sha256:([0-9a-f]{64})$/i.exec(asset?.digest ?? "");
  return match ? match[1].toLowerCase() : null;
}

/**
 * The SHA-256 every asset of a release must have, from GitHub's own digest and
 * the release's SHA256SUMS. Refuses a release whose two records disagree, an
 * asset neither vouches for, and a SHA256SUMS line for a file the release does
 * not carry (an incomplete upload).
 */
export function expectedDigests(assets, sumsText) {
  const sums = sumsText == null ? new Map() : parseSha256Sums(sumsText);
  const names = new Set(assets.map((asset) => asset.name));
  const absent = [...sums.keys()].filter((name) => !names.has(name));
  if (absent.length) throw new Error(`SHA256SUMS lists files the release does not have: ${absent.join(", ")}`);
  const expected = new Map();
  for (const asset of assets) {
    assertAssetName(asset.name);
    const fromGithub = githubDigest(asset);
    const fromSums = sums.get(asset.name) ?? null;
    if (fromGithub && fromSums && fromGithub !== fromSums) {
      throw new Error(`${asset.name}: GitHub records ${fromGithub} but SHA256SUMS says ${fromSums}`);
    }
    const sha256 = fromGithub ?? fromSums;
    if (!sha256) throw new Error(`${asset.name}: neither GitHub nor SHA256SUMS records its SHA-256`);
    expected.set(asset.name, sha256);
  }
  return expected;
}

/**
 * latest.json: the fields of GitHub's release object that the home page and the
 * in-app updater read, so either can take it where it would take the API's
 * answer. `browser_download_url` points at the mirror; `github_download_url`
 * keeps the original as a fallback.
 */
export function buildManifest(release, digests, mirroredAt) {
  const assets = release.assets.map((asset) => {
    const sha256 = digests.get(asset.name);
    if (!sha256 || !SHA256_PATTERN.test(sha256)) throw new Error(`${asset.name}: no verified SHA-256`);
    return {
      name: asset.name,
      size: asset.size,
      digest: `sha256:${sha256}`,
      content_type: objectHeaders(asset.name)["content-type"],
      browser_download_url: mirrorUrl(release.tag_name, asset.name),
      github_download_url: githubDownloadUrl(release.tag_name, asset.name),
    };
  });
  return {
    tag_name: release.tag_name,
    name: release.name ?? release.tag_name,
    html_url: release.html_url,
    published_at: release.published_at ?? null,
    draft: false,
    prerelease: Boolean(release.prerelease),
    body: release.body ?? "",
    assets,
    mirrored_at: mirroredAt,
  };
}

// ---------------------------------------------------------------------------
// R2's S3 API: credentials and AWS Signature Version 4

const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const hmac = (key, data) => createHmac("sha256", key).update(data).digest();

/**
 * S3 credentials from a Cloudflare API token that holds R2 permissions: the
 * token's id is the access key, the SHA-256 of its value the secret
 * (developers.cloudflare.com/r2/api/tokens). One token then serves both the
 * S3 upload and the cache purge.
 */
export function credentialsFromApiToken(tokenId, tokenValue) {
  if (!tokenId || !tokenValue) throw new Error("an API token id and value are both required");
  return { accessKeyId: tokenId, secretAccessKey: sha256Hex(tokenValue) };
}

/** RFC 3986 encoding, which SigV4 requires: only unreserved characters stay as they are. */
function encodeRfc3986(text) {
  return encodeURIComponent(text).replace(/[!'()*]/g, (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`);
}

/** `/bucket/key` with every segment encoded once, which is S3's canonical URI. */
export function canonicalPath(...segments) {
  return `/${segments.flatMap((segment) => segment.split("/")).map(encodeRfc3986).join("/")}`;
}

function amzDate(now) {
  return now.toISOString().replace(/[:-]|\.\d{3}/g, "");
}

/**
 * Signs one S3 request. `url` is already in canonical form (see
 * `canonicalPath`); every header passed is signed. Returns the headers to send,
 * including `authorization`, `x-amz-date` and `x-amz-content-sha256`.
 */
export function signS3Request({ method, url, headers = {}, payloadSha256, credentials, region = "auto", now = new Date() }) {
  const target = new URL(url);
  const timestamp = amzDate(now);
  const day = timestamp.slice(0, 8);
  const all = {
    ...Object.fromEntries(Object.entries(headers).map(([name, value]) => [name.toLowerCase(), String(value).trim()])),
    host: target.host,
    "x-amz-content-sha256": payloadSha256,
    "x-amz-date": timestamp,
  };
  const names = Object.keys(all).sort();
  const query = [...target.searchParams]
    .map(([name, value]) => [encodeRfc3986(name), encodeRfc3986(value)])
    .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
    .map(([name, value]) => `${name}=${value}`)
    .join("&");
  const canonicalRequest = [
    method,
    target.pathname,
    query,
    names.map((name) => `${name}:${all[name]}\n`).join(""),
    names.join(";"),
    payloadSha256,
  ].join("\n");
  const scope = `${day}/${region}/s3/aws4_request`;
  const stringToSign = ["AWS4-HMAC-SHA256", timestamp, scope, sha256Hex(canonicalRequest)].join("\n");
  const signingKey = ["s3", "aws4_request"].reduce(
    (key, part) => hmac(key, part),
    hmac(hmac(`AWS4${credentials.secretAccessKey}`, day), region)
  );
  const signature = createHmac("sha256", signingKey).update(stringToSign).digest("hex");
  const { host: _host, ...sent } = all;
  return {
    ...sent,
    authorization: `AWS4-HMAC-SHA256 Credential=${credentials.accessKeyId}/${scope}, SignedHeaders=${names.join(";")}, Signature=${signature}`,
  };
}
