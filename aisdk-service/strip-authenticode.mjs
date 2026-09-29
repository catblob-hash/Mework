// Removes the Authenticode signature from a Windows PE image, as `signtool
// remove /s` does, without needing the Windows SDK.
//
// Node's SEA steps on Windows start from a copy of the signed `node.exe`, and
// postject rewrites the image around the signature instead of dropping it. What
// comes out carries a certificate table that no longer describes the file:
// Windows runs it, but `signtool` refuses to sign it (0x800700C1,
// ERROR_BAD_EXE_FORMAT), and so does anything that signs a package containing
// it — an MSIX, and the Microsoft Store's own re-signing. Stripped first, the
// sidecar comes out simply unsigned.

const IMAGE_DOS_SIGNATURE = 0x5a4d; // "MZ"
const IMAGE_NT_SIGNATURE = 0x00004550; // "PE\0\0"
const PE32 = 0x10b;
const PE32_PLUS = 0x20b;
/** IMAGE_DIRECTORY_ENTRY_SECURITY. */
const SECURITY_DIRECTORY = 4;

/**
 * `image` without its certificate table: the security data directory entry
 * zeroed and the table (always the last thing in a signed image) cut off. An
 * image with no signature is returned as it is.
 */
export function stripAuthenticode(image) {
  if (image.length < 0x40 || image.readUInt16LE(0) !== IMAGE_DOS_SIGNATURE) {
    throw new Error("not a PE image: no MZ header");
  }
  const header = image.readUInt32LE(0x3c);
  if (header + 24 > image.length || image.readUInt32LE(header) !== IMAGE_NT_SIGNATURE) {
    throw new Error("not a PE image: no PE signature");
  }
  // The optional header follows the 4-byte signature and the 20-byte COFF header.
  const optional = header + 24;
  const magic = image.readUInt16LE(optional);
  if (magic !== PE32 && magic !== PE32_PLUS) {
    throw new Error(`not a PE image: optional header magic 0x${magic.toString(16)}`);
  }
  const directories = optional + (magic === PE32_PLUS ? 112 : 96);
  const directoryCount = image.readUInt32LE(directories - 4);
  if (directoryCount <= SECURITY_DIRECTORY) return image;
  const entry = directories + SECURITY_DIRECTORY * 8;
  // For this one directory the "address" is a file offset, not an RVA.
  const offset = image.readUInt32LE(entry);
  const size = image.readUInt32LE(entry + 4);
  if (offset === 0 && size === 0) return image;
  if (offset < entry + 8 || offset + size > image.length) {
    throw new Error(`the certificate table (${offset}+${size}) lies outside the ${image.length}-byte image`);
  }
  const stripped = Buffer.from(image.subarray(0, offset));
  stripped.writeUInt32LE(0, entry);
  stripped.writeUInt32LE(0, entry + 4);
  return stripped;
}
