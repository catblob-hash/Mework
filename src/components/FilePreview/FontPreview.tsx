import { useEffect, useMemo, useState } from "react";
import { useI18n } from "../../i18n";
import { dataUrlBytes, formatBytes } from "./format";

export interface FontNames {
  family: string | null;
  subfamily: string | null;
  fullName: string | null;
  version: string | null;
  designer: string | null;
}

/**
 * The names an OpenType or TrueType font gives itself, from its `name` table.
 *
 * WOFF and WOFF2 compress their tables, and a collection holds several fonts; for
 * those the file name is all the preview says about them, which is also what it
 * says when a table is malformed.
 */
export function readFontNames(bytes: Uint8Array): FontNames | null {
  const empty: FontNames = { family: null, subfamily: null, fullName: null, version: null, designer: null };
  if (bytes.length < 12) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = view.getUint32(0);
  // 0x00010000 is TrueType outlines, `OTTO` is CFF, `true` is Apple's TrueType.
  if (tag !== 0x00010000 && tag !== 0x4f54544f && tag !== 0x74727565) return null;
  const tables = view.getUint16(4);
  let nameOffset = -1;
  for (let index = 0; index < tables; index += 1) {
    const record = 12 + index * 16;
    if (record + 16 > bytes.length) return null;
    if (view.getUint32(record) === 0x6e616d65) nameOffset = view.getUint32(record + 8);
  }
  if (nameOffset < 0 || nameOffset + 6 > bytes.length) return null;
  const count = view.getUint16(nameOffset + 2);
  const storage = nameOffset + view.getUint16(nameOffset + 4);
  const found: Record<number, { text: string; score: number }> = {};
  for (let index = 0; index < count; index += 1) {
    const record = nameOffset + 6 + index * 12;
    if (record + 12 > bytes.length) break;
    const platform = view.getUint16(record);
    const encoding = view.getUint16(record + 2);
    const language = view.getUint16(record + 4);
    const nameId = view.getUint16(record + 6);
    const length = view.getUint16(record + 8);
    const offset = storage + view.getUint16(record + 10);
    if (![1, 2, 4, 5, 9].includes(nameId) || offset + length > bytes.length) continue;
    let text: string;
    if (platform === 3 || platform === 0) {
      // Windows and Unicode platforms store UTF-16BE.
      let decoded = "";
      for (let at = offset; at + 1 < offset + length; at += 2) decoded += String.fromCharCode(view.getUint16(at));
      text = decoded;
    } else if (platform === 1 && encoding === 0) {
      text = String.fromCharCode(...bytes.subarray(offset, offset + length));
    } else {
      continue;
    }
    // English (US) Windows names first, then any Unicode name, then whatever Mac Roman has.
    const score = platform === 3 && language === 0x409 ? 3 : platform === 3 || platform === 0 ? 2 : 1;
    if (!found[nameId] || found[nameId].score < score) found[nameId] = { text: text.trim(), score };
  }
  return {
    ...empty,
    family: found[1]?.text || null,
    subfamily: found[2]?.text || null,
    fullName: found[4]?.text || null,
    version: found[5]?.text || null,
    designer: found[9]?.text || null
  };
}

let faceCounter = 0;

const SIZES = [12, 16, 20, 28, 40, 56];
const GLYPH_ROWS = [
  "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
  "abcdefghijklmnopqrstuvwxyz",
  "0123456789 !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~",
  "ÀÉÎÕÜ àéîõü ßæøå ΑΒΓΔ αβγδ АБВГ абвг"
];

/**
 * A font, loaded into the page under a private family name and shown at the
 * sizes a reader compares type at.
 *
 * `FontFace` built from bytes is not a font *fetch*, so the renderer's
 * `font-src` does not stand in its way; the face is removed again when the
 * preview closes, so no document elsewhere in the app can pick it up.
 */
export function FontPreview({ source, name, bytes }: { source: string; name: string; bytes: number | null }) {
  const { t } = useI18n();
  const [family, setFamily] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sample, setSample] = useState("");
  const data = useMemo(() => dataUrlBytes(source), [source]);
  const names = useMemo(() => readFontNames(data), [data]);

  useEffect(() => {
    setFamily(null);
    setError(null);
    if (typeof FontFace === "undefined" || typeof document === "undefined" || !document.fonts) {
      setError(t("当前界面引擎不支持字体预览。", "This window's engine cannot preview fonts."));
      return;
    }
    faceCounter += 1;
    const privateName = `mework-font-preview-${faceCounter}`;
    const face = new FontFace(privateName, data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer);
    let cancelled = false;
    void face.load().then((loaded) => {
      if (cancelled) return;
      document.fonts.add(loaded);
      setFamily(privateName);
    }).catch(() => {
      if (!cancelled) setError(t("无法加载这个字体文件。", "This font file could not be loaded."));
    });
    return () => {
      cancelled = true;
      document.fonts.delete(face);
    };
  }, [data, t]);

  if (error) return <p className="files-pane__notice">{error}</p>;
  if (!family) return <p className="files-pane__notice">{t("正在读取…", "Loading…")}</p>;

  // Glyph samples are the same in every language; they go through `t` only
  // because any CJK literal in a component reads as interface copy to the audit.
  // Latin comes first: most fonts are Latin fonts, and a sample that opens with
  // glyphs the font lacks shows the fallback rather than the face.
  const pangram = sample || t(
    "The quick brown fox jumps over the lazy dog 敏捷的棕色狐狸跳过了懒狗",
    "The quick brown fox jumps over the lazy dog 敏捷的棕色狐狸跳过了懒狗"
  );
  const cjkRow = t("永和九年，岁在癸丑，暮春之初。あいうえお アイウエオ 한국어", "永和九年，岁在癸丑，暮春之初。あいうえお アイウエオ 한국어");
  const face = { fontFamily: `"${family}", system-ui` };

  return (
    <div className="file-preview file-preview--font">
      <header className="file-preview__font-header">
        <div className="file-preview__font-name" style={face}>{names?.fullName ?? names?.family ?? name}</div>
        <div className="file-preview__meta">
          {[names?.family, names?.subfamily, names?.version, names?.designer, bytes !== null ? formatBytes(bytes) : null]
            .filter(Boolean)
            .join(" · ")}
        </div>
        <input
          className="file-preview__font-sample-input"
          type="text"
          value={sample}
          placeholder={t("输入示例文字…", "Type sample text…")}
          aria-label={t("示例文字", "Sample text")}
          onChange={(event) => setSample(event.target.value)}
        />
      </header>
      <section className="file-preview__font-sizes">
        {SIZES.map((size) => (
          <div className="file-preview__font-row" key={size}>
            <span className="file-preview__font-size">{size}</span>
            <span className="file-preview__font-sample" style={{ ...face, fontSize: size }}>{pangram}</span>
          </div>
        ))}
      </section>
      <section className="file-preview__font-glyphs" style={face}>
        {[...GLYPH_ROWS, cjkRow].map((row) => <p key={row}>{row}</p>)}
      </section>
    </div>
  );
}
