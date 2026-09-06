/**
 * Cat loafing on the top-right rim of a draft composer.
 *
 * The drawing is laid out around a ledge line at y=28 in the 66x42 viewBox:
 * everything above it is the cat, everything below it — only the tail — hangs in
 * front of the box. `.composer-cat` in orchestration.css lines that ledge up
 * with the composer's top border, which is what sells the cat as resting on the
 * edge rather than floating above it. Moving one without the other breaks the
 * pose, and the transform origins over there are in these coordinates too.
 *
 * This is a solid silhouette, so every part has to overlap the mass it belongs
 * to or it reads as a detached blob: both ear bases and the muzzle sit inside
 * the head circle, the paw overlaps the chest, and the chest bridges the head to
 * the back. Conversely, anything drawn strictly inside the outline — an eye, the
 * far-side legs — is invisible and not worth drawing. The one shape that has to
 * stay clear of the mass is the head: drop it level with the back and the cat
 * reads as a slug with ears, so the neck notch between the head circle and the
 * back's leading curve is load-bearing, not decoration.
 */
export function ComposerCat() {
  return (
    <svg className="composer-cat" viewBox="0 0 66 42" aria-hidden="true">
      <g className="composer-cat__figure">
        <path className="composer-cat__tail" d="M53 22C60 22 63.5 27 60.5 35" />
        <path className="composer-cat__back" d="M16 28L16 19C21 13.5 35 11 45 14.5C52 17.5 56 22 55.5 28Z" />
        <rect className="composer-cat__chest" x="10" y="14" width="12" height="14" rx="5.5" />
        <rect className="composer-cat__paw" x="6.5" y="23" width="10.5" height="5" rx="2.5" />
        <g className="composer-cat__head">
          <path className="composer-cat__ear composer-cat__ear--rear" d="M22 7.9L21.4 2.25L17.1 4.7Z" />
          <path className="composer-cat__ear composer-cat__ear--front" d="M14.7 4.7L10.75 2.06L10 7.22Z" />
          <circle cx="16" cy="12" r="7.5" />
          <ellipse cx="9.8" cy="14" rx="3.4" ry="2.7" />
        </g>
      </g>
    </svg>
  );
}
