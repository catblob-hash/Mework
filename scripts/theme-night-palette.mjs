/*
 * The one rule that relates the day palette to the night palette.
 *
 * Night is the day colour mirrored across the middle of the lightness axis:
 * HSL hue and saturation are carried over untouched and only `L` becomes
 * `1 - L`. Because HSL chroma is `(1 - |2L - 1|) * S`, mirroring `L` leaves
 * chroma numerically identical, so a colour keeps both its hue and its
 * intensity and only changes which end of the light/dark axis it sits on.
 *
 * That is what separates this from a per-channel inverse (`255 - channel`),
 * which is a rotation to the opposite hue: it turned every red teal and every
 * green pink, so nothing that carried meaning by its colour survived the
 * switch. Under the mirror rule, surfaces and text trade places — a near-white
 * page becomes a near-black one — while red stays red, green stays green, and
 * the warm grey the product is built out of stays warm. A colour already at
 * the middle of the axis (`--danger`, `#c43d3d`, L = 50%) comes through
 * completely unchanged.
 *
 * `scripts/check-theme-colors.mjs` enforces the rule and
 * `scripts/write-theme-night-palette.mjs` regenerates the night block from it,
 * so both sides of the palette always agree on exactly this function.
 */

/** Parse a `#rgb`/`#rrggbb`(`aa`) or `rgb()`/`rgba()` colour into channels plus an opaque alpha tag. */
export function parseColor(value, token) {
  const normalized = value.trim().toLowerCase();
  if (normalized.startsWith("#")) {
    let hex = normalized.slice(1);
    if (hex.length === 3 || hex.length === 4) {
      hex = [...hex].map((character) => character.repeat(2)).join("");
    }
    if (hex.length !== 6 && hex.length !== 8) {
      throw new Error(`${token} has an unsupported hex color`);
    }
    return {
      red: Number.parseInt(hex.slice(0, 2), 16),
      green: Number.parseInt(hex.slice(2, 4), 16),
      blue: Number.parseInt(hex.slice(4, 6), 16),
      alpha: hex.length === 8 ? `hex:${hex.slice(6, 8)}` : null
    };
  }
  const rgb = normalized.match(/^rgba?\((.*)\)$/);
  if (!rgb) throw new Error(`${token} must be a hex or rgb() color`);
  const body = rgb[1].trim();
  let channels;
  let alpha = null;
  if (body.includes(",")) {
    const parts = body.split(",").map((part) => part.trim());
    if (parts.length !== 3 && parts.length !== 4) {
      throw new Error(`${token} has an unsupported rgb() color`);
    }
    channels = parts.slice(0, 3);
    alpha = parts[3] ?? null;
  } else {
    const [channelSource, alphaSource, ...extra] = body.split("/").map((part) => part.trim());
    if (extra.length) throw new Error(`${token} has an unsupported rgb() color`);
    channels = channelSource.split(/\s+/);
    alpha = alphaSource || null;
  }
  if (channels.length !== 3) throw new Error(`${token} has an unsupported rgb() color`);
  return {
    red: parseChannel(channels[0], token),
    green: parseChannel(channels[1], token),
    blue: parseChannel(channels[2], token),
    alpha: alpha === null ? null : `rgb:${alpha}`
  };
}

function parseChannel(value, token) {
  if (value.endsWith("%")) {
    const percent = Number(value.slice(0, -1));
    if (!Number.isFinite(percent) || percent < 0 || percent > 100) {
      throw new Error(`${token} has an invalid RGB percentage channel`);
    }
    return Math.round(percent * 255 / 100);
  }
  const channel = Number(value);
  if (!Number.isInteger(channel) || channel < 0 || channel > 255) {
    throw new Error(`${token} must use integer RGB channels from 0 through 255`);
  }
  return channel;
}

function toHsl({ red, green, blue }) {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const chroma = max - min;
  const lightness = (max + min) / 2;
  if (chroma === 0) return { hue: 0, saturation: 0, lightness };
  const saturation = chroma / (1 - Math.abs(2 * lightness - 1));
  let hue;
  if (max === r) hue = ((g - b) / chroma) % 6;
  else if (max === g) hue = (b - r) / chroma + 2;
  else hue = (r - g) / chroma + 4;
  return { hue: (hue * 60 + 360) % 360, saturation, lightness };
}

function fromHsl({ hue, saturation, lightness }) {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const sector = hue / 60;
  const second = chroma * (1 - Math.abs((sector % 2) - 1));
  const [r, g, b] = sector < 1 ? [chroma, second, 0]
    : sector < 2 ? [second, chroma, 0]
    : sector < 3 ? [0, chroma, second]
    : sector < 4 ? [0, second, chroma]
    : sector < 5 ? [second, 0, chroma]
    : [chroma, 0, second];
  const base = lightness - chroma / 2;
  return {
    red: Math.round((r + base) * 255),
    green: Math.round((g + base) * 255),
    blue: Math.round((b + base) * 255)
  };
}

/** The night counterpart of a day colour: same hue and chroma, mirrored lightness. */
export function nightFromDay(dayColor) {
  const hsl = toHsl(dayColor);
  return { ...fromHsl({ ...hsl, lightness: 1 - hsl.lightness }), alpha: dayColor.alpha };
}

/** Render a colour the way the palette writes it: hex when opaque, `rgb(r g b / a)` when not. */
export function formatColor({ red, green, blue, alpha }, dayValue) {
  const hex = [red, green, blue].map((channel) => channel.toString(16).padStart(2, "0")).join("");
  if (alpha === null) return `#${hex}`;
  if (alpha.startsWith("hex:")) return `#${hex}${alpha.slice(4)}`;
  // Day values in the palette are authored in the modern space-separated form;
  // mirror whichever form the day side used so the two blocks stay comparable.
  const slash = alpha.slice(4);
  return dayValue?.includes(",")
    ? `rgba(${red}, ${green}, ${blue}, ${slash})`
    : `rgb(${red} ${green} ${blue} / ${slash})`;
}
