import assert from "node:assert/strict";
import test from "node:test";

import { stripAuthenticode } from "../../aisdk-service/strip-authenticode.mjs";

const HEADER = 0x80;

/** A PE32+ image skeleton: headers, `body` bytes, then `certificate` as its certificate table. */
function image({ body = 64, certificate = null, magic = 0x20b } = {}) {
  const optionalSize = magic === 0x20b ? 240 : 224;
  const headersEnd = HEADER + 24 + optionalSize;
  const length = headersEnd + body + (certificate?.length ?? 0);
  const bytes = Buffer.alloc(length, 0xab);
  bytes.writeUInt16LE(0x5a4d, 0);
  bytes.writeUInt32LE(HEADER, 0x3c);
  bytes.writeUInt32LE(0x00004550, HEADER);
  const optional = HEADER + 24;
  bytes.writeUInt16LE(magic, optional);
  const directories = optional + (magic === 0x20b ? 112 : 96);
  bytes.writeUInt32LE(16, directories - 4);
  bytes.fill(0, directories, directories + 16 * 8);
  if (certificate) {
    const offset = headersEnd + body;
    certificate.copy(bytes, offset);
    bytes.writeUInt32LE(offset, directories + 4 * 8);
    bytes.writeUInt32LE(certificate.length, directories + 4 * 8 + 4);
  }
  return { bytes, entry: directories + 4 * 8, unsignedLength: headersEnd + body };
}

test("the certificate table is cut off and its directory entry zeroed", () => {
  for (const magic of [0x20b, 0x10b]) {
    const { bytes, entry, unsignedLength } = image({ magic, certificate: Buffer.from("signature bytes!") });
    const stripped = stripAuthenticode(bytes);
    assert.equal(stripped.length, unsignedLength);
    assert.equal(stripped.readUInt32LE(entry), 0);
    assert.equal(stripped.readUInt32LE(entry + 4), 0);
    // Everything before the table is untouched.
    assert.deepEqual(stripped.subarray(0, entry), bytes.subarray(0, entry));
    assert.deepEqual(stripped.subarray(entry + 8), bytes.subarray(entry + 8, unsignedLength));
    // Stripping twice changes nothing more.
    assert.deepEqual(stripAuthenticode(stripped), stripped);
  }
});

test("an unsigned image comes back as it is", () => {
  const { bytes } = image();
  assert.equal(stripAuthenticode(bytes), bytes);
});

test("what is not a PE image, or a table outside it, is refused", () => {
  assert.throws(() => stripAuthenticode(Buffer.alloc(0x100)), /MZ header/u);
  const { bytes } = image();
  const noPe = Buffer.from(bytes);
  noPe.writeUInt32LE(0, HEADER);
  assert.throws(() => stripAuthenticode(noPe), /PE signature/u);
  const { bytes: signed, entry } = image({ certificate: Buffer.alloc(16) });
  const beyond = Buffer.from(signed);
  beyond.writeUInt32LE(64, entry + 4);
  assert.throws(() => stripAuthenticode(beyond), /outside/u);
});
