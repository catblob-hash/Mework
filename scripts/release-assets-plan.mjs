// Defines the canonical GitHub Release asset names and SHA-256 manifest format.
//
// The in-app updater matches downloadable assets by these names and verifies a
// download against SHA256SUMS when the manifest is present.

const SHA256_PATTERN = /^[0-9a-fA-F]{64}$/;
const ASSET_NAME_PATTERN = /^\S+$/;

function normalizeEntry(entry) {
  if (!entry || typeof entry !== "object") {
    throw new TypeError("Each checksum entry must be an object");
  }
  const { name, sha256 } = entry;
  if (typeof name !== "string" || !ASSET_NAME_PATTERN.test(name) || name.includes("/") || name.includes("\\")) {
    throw new TypeError("Checksum asset name must be non-empty and contain no whitespace or path separators");
  }
  if (typeof sha256 !== "string" || !SHA256_PATTERN.test(sha256)) {
    throw new TypeError("SHA-256 must be exactly 64 hexadecimal characters");
  }
  return { name, sha256: sha256.toLowerCase() };
}

export function releaseAssetNames(version) {
  return {
    installer: `Mework_${version}_x64-setup.exe`,
    portable: `Mework_${version}_x64_portable.zip`,
    checksums: "SHA256SUMS"
  };
}

export function formatSha256Sums(entries) {
  if (!Array.isArray(entries)) throw new TypeError("Checksum entries must be an array");
  return [...entries]
    .map(normalizeEntry)
    .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0))
    .map(({ name, sha256 }) => `${sha256}  ${name}`)
    .join("\n") + (entries.length > 0 ? "\n" : "");
}

export function parseSha256Sums(text) {
  if (typeof text !== "string") throw new TypeError("SHA256SUMS text must be a string");

  const entries = [];
  for (const line of text.split(/\r?\n/)) {
    if (/^\s*(?:#.*)?$/.test(line)) continue;
    const match = /^([0-9a-fA-F]{64}) (?: |\*)(\S+)$/.exec(line);
    if (!match) throw new TypeError(`Malformed SHA256SUMS line: ${line}`);
    entries.push(normalizeEntry({ sha256: match[1], name: match[2] }));
  }
  return entries;
}
