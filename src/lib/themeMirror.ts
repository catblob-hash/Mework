/**
 * The day → night colour rule, for the handful of colours that have to exist as
 * concrete values in TypeScript rather than as palette tokens — xterm takes a
 * theme object, not CSS variables.
 *
 * It is the same rule `src/palette.css` is generated with: hue and saturation
 * carry over untouched and only HSL lightness becomes `1 - L`, which leaves
 * chroma numerically identical. A light surface turns dark and dark text turns
 * light, but nothing changes hue, so the terminal's amber selection stays amber
 * instead of landing on its complement the way a per-channel inverse left it.
 *
 * `themeMirror.test.ts` checks this function against the night block of
 * `src/palette.css`, so the two copies of the rule cannot drift apart.
 */

interface Rgb {
  red: number;
  green: number;
  blue: number;
}

function toHsl({ red, green, blue }: Rgb): { hue: number; saturation: number; lightness: number } {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const chroma = max - min;
  const lightness = (max + min) / 2;
  if (chroma === 0) return { hue: 0, saturation: 0, lightness };
  const saturation = chroma / (1 - Math.abs(2 * lightness - 1));
  const hue = max === r
    ? ((g - b) / chroma) % 6
    : max === g
      ? (b - r) / chroma + 2
      : (r - g) / chroma + 4;
  return { hue: (hue * 60 + 360) % 360, saturation, lightness };
}

function fromHsl(hue: number, saturation: number, lightness: number): Rgb {
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
    blue: Math.round((b + base) * 255),
  };
}

/**
 * The night counterpart of a `#rrggbb` or `#rrggbbaa` colour: same hue and
 * chroma, mirrored lightness, alpha untouched. Anything else is returned as is.
 */
export function mirrorLightnessHex(value: string): string {
  const match = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(value);
  if (!match) return value;
  const day: Rgb = {
    red: Number.parseInt(match[1].slice(0, 2), 16),
    green: Number.parseInt(match[1].slice(2, 4), 16),
    blue: Number.parseInt(match[1].slice(4, 6), 16),
  };
  const hsl = toHsl(day);
  const night = fromHsl(hsl.hue, hsl.saturation, 1 - hsl.lightness);
  const hex = [night.red, night.green, night.blue]
    .map((channel) => channel.toString(16).padStart(2, "0"))
    .join("");
  return `#${hex}${match[2] ?? ""}`;
}
