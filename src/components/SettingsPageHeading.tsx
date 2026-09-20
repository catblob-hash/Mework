/** Shared heading for settings-page sections. */
export function SettingsPageHeading({
  title,
  description,
  action
}: {
  title: React.ReactNode;
  description: string;
  action?: React.ReactNode;
}) {
  return (
    <header className="settings-page-heading">
      <div><h3>{title}</h3><p>{description}</p></div>
      {action}
    </header>
  );
}
