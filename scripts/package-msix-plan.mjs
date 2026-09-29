// Pure parts of `scripts/package-msix.mjs`: the package version, the identity
// check, and filling the manifest template. Kept apart so they are tested
// without a Windows SDK.

/**
 * MSIX versions are four 16-bit numbers, and the Microsoft Store requires the
 * last one to be 0. `1.2.3` becomes `1.2.3.0`; a prerelease has no MSIX form.
 */
export function msixVersion(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/u.exec(String(version).trim());
  if (!match) {
    throw new Error(`${version} is not a plain major.minor.patch version; MSIX has no prerelease form`);
  }
  const parts = match.slice(1).map(Number);
  if (parts.some((part) => part > 65535)) throw new Error(`${version} has a part above 65535`);
  return `${parts.join(".")}.0`;
}

/**
 * The package identity the Microsoft Store assigned (Partner Center → the app →
 * Product management → Product identity). A package whose identity differs is
 * refused on upload, and the Publisher is also what a sideloading signature's
 * certificate subject must equal.
 */
export function validateIdentity(identity) {
  if (!identity || typeof identity !== "object") throw new TypeError("identity must be an object");
  const { name, publisher, publisherDisplayName } = identity;
  // Package/Identity/Name: 3–50 characters of letters, digits, '.' and '-'.
  if (typeof name !== "string" || !/^[A-Za-z0-9.-]{3,50}$/u.test(name)) {
    throw new Error("identity.name must be the Package/Identity/Name from Partner Center");
  }
  if (typeof publisher !== "string" || !/^CN=\S/u.test(publisher)) {
    throw new Error("identity.publisher must be the Package/Identity/Publisher from Partner Center (CN=…)");
  }
  if (typeof publisherDisplayName !== "string" || publisherDisplayName.trim() === "") {
    throw new Error("identity.publisherDisplayName must be the PublisherDisplayName from Partner Center");
  }
  return { name, publisher, publisherDisplayName: publisherDisplayName.trim() };
}

function escapeXml(text) {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

/** Fills every `{{FIELD}}` of the template; a field left over is an error. */
export function renderManifest(template, { identity, version }) {
  const fields = {
    IDENTITY_NAME: identity.name,
    PUBLISHER: identity.publisher,
    PUBLISHER_DISPLAY_NAME: identity.publisherDisplayName,
    VERSION: version
  };
  const rendered = template.replace(/\{\{([A-Z_]+)\}\}/gu, (whole, field) => {
    if (!(field in fields)) throw new Error(`the manifest template has an unknown field ${whole}`);
    return escapeXml(fields[field]);
  });
  return rendered;
}

/** `Mework_1.0.0_x64.msix`, beside the installer and portable names in release-assets-plan.mjs. */
export function msixFileNames(version) {
  return {
    signed: `Mework_${version}_x64.msix`,
    store: `Mework_${version}_x64_store.msix`,
    certificate: "Mework_msix_signing.cer"
  };
}
