// Copies a GitHub release to the download mirror at https://dl.mework.dev (a
// Cloudflare R2 bucket behind that custom domain). Layout and verification
// rules are in release-mirror-plan.mjs.
//
// Usage:
//   node scripts/release-mirror.mjs [--tag v1.0.0] [--dry-run]
//
// Without --tag it mirrors the latest release. latest.json is rewritten only
// when the mirrored tag is GitHub's latest release, and only after every file
// is uploaded and readable at its public address. --dry-run downloads and
// verifies the files and stops before touching R2, so it needs no Cloudflare
// credentials.
//
// Environment:
//   GITHUB_TOKEN           optional; raises the GitHub API quota
//   CLOUDFLARE_API_TOKEN   an API token with Workers R2 Storage: Edit (the S3
//                          credentials are derived from it) and, for the purge,
//                          Cache Purge on the mework.dev zone
//   CLOUDFLARE_ACCOUNT_ID  the account that owns the bucket
//   CLOUDFLARE_ZONE_ID     the mework.dev zone; without it a run that replaces
//                          a stored file cannot purge the CDN's copy, and fails
//                          before latest.json is written
//   R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY  optional S3 credentials to use
//                          instead of deriving them from the API token
//
// The bucket itself is set up once by hand (see the commands in
// .github/workflows/release-mirror.yml).

import { createHash } from "node:crypto";
import fs from "node:fs";
import process from "node:process";

import {
  MANIFEST_HEADERS,
  MIRROR,
  buildManifest,
  canonicalPath,
  credentialsFromApiToken,
  expectedDigests,
  mirrorUrl,
  objectHeaders,
  objectKey,
  signS3Request
} from "./release-mirror-plan.mjs";

const label = "[release:mirror]";
const repository = process.env.GITHUB_REPOSITORY || MIRROR.repository;
const TRANSFER_TIMEOUT = 20 * 60 * 1000;
const API_TIMEOUT = 30 * 1000;

function fail(message) {
  console.error(`${label} ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const options = { tag: null, dryRun: false };
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--dry-run") options.dryRun = true;
    else if (argv[index] === "--tag" && argv[index + 1]) options.tag = argv[++index];
    else fail(`Unsupported argument: ${argv[index]}`);
  }
  return options;
}

const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const megabytes = (bytes) => `${(bytes / 1e6).toFixed(1)} MB`;

/** A request retried twice on a network error or a 5xx, the failures a CI run meets by chance. */
async function request(url, init = {}, timeout = API_TIMEOUT) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      const response = await fetch(url, { ...init, signal: AbortSignal.timeout(timeout) });
      if (response.status < 500 || attempt === 3) return response;
      console.warn(`${label} ${init.method ?? "GET"} ${url}: HTTP ${response.status}, retrying`);
    } catch (error) {
      if (attempt === 3) throw new Error(`${init.method ?? "GET"} ${url}: ${error.message}`);
      console.warn(`${label} ${init.method ?? "GET"} ${url}: ${error.message}, retrying`);
    }
    await new Promise((resolve) => setTimeout(resolve, attempt * 5000));
  }
}

async function json(response, what) {
  if (!response.ok) throw new Error(`${what}: HTTP ${response.status} ${(await response.text()).slice(0, 300)}`);
  return response.json();
}

// --- GitHub -----------------------------------------------------------------

function githubHeaders() {
  const headers = { accept: "application/vnd.github+json", "user-agent": "mework-release-mirror", "x-github-api-version": "2022-11-28" };
  if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  return headers;
}

async function githubRelease(path) {
  return json(await request(`https://api.github.com/repos/${repository}/releases/${path}`, { headers: githubHeaders() }), `GitHub ${path}`);
}

/** The asset's bytes from GitHub's public download address. */
async function download(asset) {
  const response = await request(asset.browser_download_url, { headers: { "user-agent": "mework-release-mirror" } }, TRANSFER_TIMEOUT);
  if (!response.ok) throw new Error(`downloading ${asset.name}: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length !== asset.size) throw new Error(`${asset.name}: GitHub sent ${bytes.length} bytes, the release says ${asset.size}`);
  return bytes;
}

// --- Cloudflare ---------------------------------------------------------------

/** The token as pasted, without the stray whitespace a paste can carry: fetch trims it from a header, but the S3 secret hashes every byte. */
function apiToken() {
  return (process.env.CLOUDFLARE_API_TOKEN ?? "").trim();
}

function cloudflareHeaders() {
  return { authorization: `Bearer ${apiToken()}`, "content-type": "application/json" };
}

/** The API token's id: a user token answers on /user, an account-owned one on its account. */
async function apiTokenId() {
  const paths = ["user/tokens/verify", `accounts/${process.env.CLOUDFLARE_ACCOUNT_ID}/tokens/verify`];
  for (const path of paths) {
    const response = await request(`https://api.cloudflare.com/client/v4/${path}`, { headers: cloudflareHeaders() });
    const body = await response.json().catch(() => ({}));
    if (response.ok && body.success && body.result?.id) {
      if (body.result.status !== "active") throw new Error(`the Cloudflare API token is ${body.result.status}`);
      console.log(`${label} API token verified as a ${path.startsWith("user") ? "user" : "account"} token`);
      return body.result.id;
    }
  }
  throw new Error("CLOUDFLARE_API_TOKEN does not verify as a user or account token");
}

async function s3Credentials() {
  if (process.env.R2_ACCESS_KEY_ID && process.env.R2_SECRET_ACCESS_KEY) {
    return { accessKeyId: process.env.R2_ACCESS_KEY_ID, secretAccessKey: process.env.R2_SECRET_ACCESS_KEY };
  }
  if (!apiToken()) throw new Error("set CLOUDFLARE_API_TOKEN (or R2_ACCESS_KEY_ID and R2_SECRET_ACCESS_KEY)");
  return credentialsFromApiToken(await apiTokenId(), apiToken());
}

function bucketUrl(key) {
  return `https://${process.env.CLOUDFLARE_ACCOUNT_ID}.r2.cloudflarestorage.com${canonicalPath(MIRROR.bucket, key)}`;
}

/** The stored object's size and recorded SHA-256, or null when there is none. */
async function headObject(credentials, key) {
  const url = bucketUrl(key);
  const empty = sha256Hex("");
  const response = await request(url, { method: "HEAD", headers: signS3Request({ method: "HEAD", url, payloadSha256: empty, credentials }) });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`HEAD ${key}: HTTP ${response.status}${await s3ErrorDetail(credentials, key)}`);
  return { size: Number(response.headers.get("content-length")), sha256: response.headers.get("x-amz-meta-sha256") };
}

/** A HEAD answer has no body; the same request as a one-byte GET says why S3 refused it. */
async function s3ErrorDetail(credentials, key) {
  const url = bucketUrl(key);
  const headers = signS3Request({ method: "GET", url, headers: { range: "bytes=0-0" }, payloadSha256: sha256Hex(""), credentials });
  const text = await (await request(url, { headers })).text().catch(() => "");
  const field = (name) => new RegExp(`<${name}>([^<]*)</${name}>`).exec(text)?.[1];
  return field("Code") ? ` (${field("Code")}: ${field("Message") ?? ""})` : "";
}

/** Uploads `bytes` signed with their SHA-256, so R2 stores them only if they arrive intact. */
async function putObject(credentials, key, bytes, sha256, headers) {
  const url = bucketUrl(key);
  const signed = signS3Request({ method: "PUT", url, headers: { ...headers, "x-amz-meta-sha256": sha256 }, payloadSha256: sha256, credentials });
  const response = await request(url, { method: "PUT", headers: signed, body: bytes }, TRANSFER_TIMEOUT);
  if (!response.ok) throw new Error(`PUT ${key}: HTTP ${response.status} ${(await response.text()).slice(0, 300)}`);
}

async function purge(urls) {
  const zone = process.env.CLOUDFLARE_ZONE_ID;
  if (!zone) throw new Error(`CLOUDFLARE_ZONE_ID is not set, so the CDN may still serve the old copy of: ${urls.join(", ")}`);
  const body = await json(
    await request(`https://api.cloudflare.com/client/v4/zones/${zone}/purge_cache`, { method: "POST", headers: cloudflareHeaders(), body: JSON.stringify({ files: urls }) }),
    "purging the CDN cache"
  );
  if (!body.success) throw new Error(`purging the CDN cache: ${JSON.stringify(body.errors)}`);
}

/** The public address must answer with the stored size, which also proves the custom domain works. */
async function checkPublic(url, size) {
  const response = await request(url, { method: "HEAD", headers: { "user-agent": "mework-release-mirror" } });
  const length = Number(response.headers.get("content-length"));
  if (!response.ok || length !== size) throw new Error(`${url}: HTTP ${response.status}, ${length} bytes instead of ${size}`);
}

// --- Run --------------------------------------------------------------------

const options = parseArguments(process.argv.slice(2));
try {
  for (const name of options.dryRun ? [] : ["CLOUDFLARE_ACCOUNT_ID"]) {
    if (!process.env[name]) throw new Error(`${name} is not set`);
  }
  const latest = await githubRelease("latest");
  const release = options.tag ? await githubRelease(`tags/${encodeURIComponent(options.tag)}`) : latest;
  if (release.draft) throw new Error(`${release.tag_name} is a draft`);
  const isLatest = release.tag_name === latest.tag_name;
  const assets = release.assets;
  if (!assets.length) throw new Error(`${release.tag_name} has no files`);
  for (const asset of assets) objectKey(release.tag_name, asset.name);
  console.log(`${label} ${repository} ${release.tag_name}${isLatest ? " (latest)" : ""}: ${assets.length} files, ${megabytes(assets.reduce((sum, asset) => sum + asset.size, 0))}`);

  // The checksum list first: every other file is checked against it.
  const sumsAsset = assets.find((asset) => asset.name === "SHA256SUMS");
  const sumsBytes = sumsAsset ? await download(sumsAsset) : null;
  const expected = expectedDigests(assets, sumsBytes?.toString("utf8") ?? null);

  const credentials = options.dryRun ? null : await s3Credentials();
  const replaced = [];
  for (const asset of assets) {
    const key = objectKey(release.tag_name, asset.name);
    const sha256 = expected.get(asset.name);
    const stored = credentials ? await headObject(credentials, key) : null;
    if (stored?.sha256 === sha256 && stored.size === asset.size) {
      console.log(`${label} ${key} already mirrored`);
      continue;
    }
    const bytes = asset === sumsAsset ? sumsBytes : await download(asset);
    const actual = sha256Hex(bytes);
    if (actual !== sha256) throw new Error(`${asset.name}: downloaded SHA-256 ${actual}, expected ${sha256}`);
    if (!credentials) {
      console.log(`${label} ${key} verified (${megabytes(asset.size)}), dry run: not uploaded`);
      continue;
    }
    await putObject(credentials, key, bytes, sha256, objectHeaders(asset.name));
    if (stored) replaced.push(mirrorUrl(release.tag_name, asset.name));
    console.log(`${label} ${key} uploaded (${megabytes(asset.size)}${stored ? ", replacing a different copy" : ""})`);
  }
  if (options.dryRun) {
    console.log(`${label} dry run complete; latest.json would ${isLatest ? "" : "not "}be rewritten`);
    process.exit(0);
  }

  // Readable at the public address before latest.json can send anyone there. A
  // replaced file is purged first, or the edge would answer with the old copy.
  if (replaced.length) await purge(replaced);
  for (const asset of assets) await checkPublic(mirrorUrl(release.tag_name, asset.name), asset.size);

  const summary = [`### ${release.tag_name} on ${MIRROR.origin}`, "", ...assets.map((asset) => `- [${asset.name}](${mirrorUrl(release.tag_name, asset.name)}) — ${megabytes(asset.size)}, \`${expected.get(asset.name)}\``)];
  if (isLatest) {
    const manifest = Buffer.from(`${JSON.stringify(buildManifest(release, expected, new Date().toISOString()), null, 2)}\n`);
    await putObject(credentials, MIRROR.manifestKey, manifest, sha256Hex(manifest), MANIFEST_HEADERS);
    await purge([`${MIRROR.origin}/${MIRROR.manifestKey}`]).catch((error) => console.warn(`${label} ${error.message}`));
    const published = await json(await request(`${MIRROR.origin}/${MIRROR.manifestKey}`, { headers: { "cache-control": "no-cache" } }), "reading latest.json back");
    if (published.tag_name !== release.tag_name) throw new Error(`latest.json still names ${published.tag_name}`);
    console.log(`${label} latest.json now names ${release.tag_name}`);
    summary.push("", `[latest.json](${MIRROR.origin}/${MIRROR.manifestKey}) names ${release.tag_name}.`);
  } else {
    console.log(`${label} ${release.tag_name} is not the latest release (${latest.tag_name}); latest.json left alone`);
  }
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${summary.join("\n")}\n`);
} catch (error) {
  fail(error.message);
}
