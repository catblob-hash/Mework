/**
 * Shared heading for settings-page sections. Without a title it is the page's
 * description alone, as a preset's and a role's window draw it: the rail already
 * names the page, and the conversation-settings pane shows its pages that way.
 */
export function SettingsPageHeading({
  title,
  description,
  action
}: {
  title?: React.ReactNode;
  description: string;
  action?: React.ReactNode;
}) {
  return (
    <header className={title === undefined
      ? "settings-page-heading settings-page-heading--untitled"
      : "settings-page-heading"}>
      <div>{title !== undefined && <h3>{title}</h3>}<p>{description}</p></div>
      {action}
    </header>
  );
}
