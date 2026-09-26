import type { JSX, ReactNode } from "react";

/** The Appearance page's card and row, shared by its sections. */
export function SettingsCard({
  title,
  label,
  children
}: {
  /** Omitted when the card's only row already names it. */
  title?: string;
  /** The card's accessible name when it has no visible title. */
  label?: string;
  children: ReactNode;
}): JSX.Element {
  return (
    <section className="settings-card" aria-label={title ? undefined : label}>
      {title && (
        <div className="settings-card__heading">
          <div>
            <span>
              <strong>{title}</strong>
            </span>
          </div>
        </div>
      )}
      <div className="appearance-settings-page__card-body">{children}</div>
    </section>
  );
}

export function SettingRow({
  title,
  description,
  children,
  vertical = false
}: {
  title: string;
  description?: string;
  children: ReactNode;
  vertical?: boolean;
}): JSX.Element {
  return (
    <div
      className={`appearance-settings-page__row${
        vertical ? " appearance-settings-page__row--vertical" : ""
      }`}
    >
      <div className="appearance-settings-page__row-copy">
        <strong>{title}</strong>
        {description && <small>{description}</small>}
      </div>
      <div className="appearance-settings-page__row-control">{children}</div>
    </div>
  );
}
