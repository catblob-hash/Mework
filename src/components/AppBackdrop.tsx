import { useEffect, useState } from "react";
import { hasCustomBackground, useAppearance } from "../lib/appearance";
import { backgroundImageData, tierCovers, useBackgroundImportGeneration } from "../lib/backgroundImage";
import type { BackgroundImageData } from "../types";

function devicePixels(): { width: number; height: number } {
  const ratio = window.devicePixelRatio || 1;
  return {
    width: Math.round(window.innerWidth * ratio),
    height: Math.round(window.innerHeight * ratio)
  };
}

/**
 * The user's picture behind the whole window (Appearance → Custom background).
 *
 * `object-fit: cover` crops it to the window's shape and never stretches it. It asks
 * the host for the smallest tier that covers the window in device pixels, and asks
 * again only when the window — or the screen it moved to — outgrows that tier. It never
 * trades down: the tier it has is already sharp at a smaller size.
 */
export function AppBackdrop() {
  const appearance = useAppearance();
  const imageId = hasCustomBackground(appearance) ? appearance.backgroundImage : "";
  const importGeneration = useBackgroundImportGeneration();
  const [shown, setShown] = useState<{ id: string; tier: BackgroundImageData } | null>(null);

  useEffect(() => {
    if (!imageId) {
      setShown(null);
      return;
    }
    // Re-read after any import, which may have restored this very id's files.
    void importGeneration;
    let disposed = false;
    let loaded: BackgroundImageData | null = null;
    let loading = false;

    const load = (): void => {
      if (disposed || loading) return;
      const { width, height } = devicePixels();
      if (loaded && (loaded.largest || tierCovers(loaded, width, height))) return;
      loading = true;
      backgroundImageData(imageId, width, height)
        .then((tier) => {
          if (disposed) return;
          if (!loaded || tier.width > loaded.width) {
            loaded = tier;
            setShown({ id: imageId, tier });
          }
          loading = false;
          // The window may have grown while this tier was on its way.
          load();
        })
        .catch(() => {
          // A picture that cannot be read leaves the glass over the plain theme colour,
          // which is still a usable window; the settings page is where it can be re-picked.
          loading = false;
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
  }, [imageId, importGeneration]);

  // The previous picture stays up until the next one has arrived, rather than blinking out.
  if (!imageId || !shown) return null;
  return (
    <div className="app-backdrop" aria-hidden="true">
      <img
        key={shown.id}
        className="app-backdrop__image"
        src={shown.tier.dataUrl}
        alt=""
        draggable={false}
        decoding="async"
      />
    </div>
  );
}
