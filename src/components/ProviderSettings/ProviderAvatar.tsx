/**
 * Initial-based provider and model avatar.
 *
 * Do not ship vendor logos. Do not generate per-ID hues: `check-theme-colors.mjs`
 * requires non-palette files to avoid raw colors.
 */
export function ProviderAvatar({
  name,
  className = ""
}: {
  name: string;
  /** Additional class name, such as the `provider-avatar--round` variant for model rows. */
  className?: string;
}) {
  const initial = Array.from(name.trim())[0] ?? "?";
  return (
    <span className={className ? `provider-avatar ${className}` : "provider-avatar"} aria-hidden="true">
      {initial.toUpperCase()}
    </span>
  );
}
