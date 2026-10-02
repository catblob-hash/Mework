import { useEffect, useState } from "react";
import { useAppearance } from "../lib/appearance";
import { parseBackground, replacesThemeGround } from "../lib/background";
import { backgroundImageData, tierCovers, useBackgroundLibraryGeneration } from "../lib/backgroundImage";
import { setBackdropPicture, watchGlassPlates } from "../lib/glassPlate";
import type { BackgroundImageData } from "../types";

function devicePixels(): { width: number; height: number } {
  const ratio = window.devicePixelRatio || 1;
  return {
    width: Math.round(window.innerWidth * ratio),
    height: Math.round(window.innerHeight * ratio)
  };
}

/**
 * The window's background (Appearance → Custom background), behind the whole window
 * whenever the panes are glass or the ground is not the theme's own.
 *
 * The layer itself paints a solid ground; a picture lies over it. `object-fit: cover`
 * crops a picture to the window's shape and never stretches it. The bundled pictures
 * come in one size, and are cropped around their cat. For an imported one it asks the host for the smallest tier that
 * covers the window in device pixels, and asks again only when the window — or the
 * screen it moved to — outgrows that tier. It never trades down: the tier it has is
 * already sharp at a smaller size.
 */
export function AppBackdrop() {
  const appearance = useAppearance();
  const libraryGeneration = useBackgroundLibraryGeneration();
  const background = parseBackground(appearance.background);
  const layered = appearance.liquidGlass || replacesThemeGround(appearance.background);
  const bundled = layered && background.kind === "builtin" ? background.picture.picture : "";
  const focus = background.kind === "builtin" ? background.picture.focus : "";
  const imported = layered && background.kind === "imported" ? background.id : "";
  const [picture, setPicture] = useState<{ key: string; src: string; focus?: string } | null>(null);

  useEffect(() => {
    if (bundled) {
      setPicture({ key: bundled, src: bundled, focus });
      return;
    }
    if (!imported) {
      setPicture(null);
      return;
    }
    // Re-read after any import, which may have restored this very id's files.
    void libraryGeneration;
    let disposed = false;
    let loaded: BackgroundImageData | null = null;
    let loading = false;

    const load = (): void => {
      if (disposed || loading) return;
      const { width, height } = devicePixels();
      if (loaded && (loaded.largest || tierCovers(loaded, width, height))) return;
      loading = true;
      backgroundImageData(imported, width, height)
        .then((tier) => {
          if (disposed) return;
          if (!loaded || tier.width > loaded.width) {
            loaded = tier;
            setPicture({ key: imported, src: tier.dataUrl });
          }
          loading = false;
          // The window may have grown while this tier was on its way.
          load();
        })
        .catch(() => {
          // A picture that cannot be read leaves the solid ground, which is still a
          // usable window; the settings page is where another can be picked.
          loading = false;
          if (!disposed && !loaded) setPicture(null);
        });
    };

    let timer = 0;
    const schedule = (): void => {
      window.clearTimeout(timer);
      timer = window.setTimeout(load, 200);
    };
    // Moving to a screen of another density changes the ratio without always resizing,
    // and the query has to be rebuilt for the new ratio each time.
    let density: MediaQueryList | null = null;
    const watchDensity = (): void => {
      density?.removeEventListener("change", onDensityChange);
      density = typeof window.matchMedia === "function"
        ? window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`)
        : null;
      density?.addEventListener("change", onDensityChange);
    };
    function onDensityChange(): void {
      watchDensity();
      schedule();
    }

    load();
    watchDensity();
    window.addEventListener("resize", schedule);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      window.removeEventListener("resize", schedule);
      density?.removeEventListener("change", onDensityChange);
    };
  }, [bundled, focus, imported, libraryGeneration]);

  // The glass is baked from the picture once it has loaded (the `<img>`'s `onLoad`), and from
  // this layer's colour while none is shown (`lib/glassPlate.ts`).
  const showsPicture = layered && picture !== null && background.kind !== "solid";
  const solid = background.kind === "solid" ? background.scheme ?? "theme" : "theme";
  useEffect(() => (layered ? watchGlassPlates() : undefined), [layered]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: a new solid ground is a new colour to read, though nothing here reads it.
  useEffect(() => {
    if (!showsPicture) setBackdropPicture(null);
  }, [showsPicture, solid]);

  if (!layered) return null;
  // Between two pictures the previous one stays up until the next has arrived, rather
  // than blinking out to the ground.
  return (
    <div
      className="app-backdrop"
      data-solid={solid}
      aria-hidden="true"
    >
      {picture && background.kind !== "solid" && (
        // biome-ignore lint/a11y/noNoninteractiveElementInteractions: `onLoad` is the picture arriving, not the user interacting.
        <img
          key={picture.key}
          className="app-backdrop__image"
          src={picture.src}
          style={picture.focus ? { objectPosition: picture.focus } : undefined}
          alt=""
          draggable={false}
          decoding="async"
          onLoad={(event) => setBackdropPicture(event.currentTarget)}
        />
      )}
    </div>
  );
}
