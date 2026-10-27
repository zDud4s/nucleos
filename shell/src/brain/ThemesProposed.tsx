import { DAEMON_NAME } from "../copy";
import { useAcceptTheme, useRefuseTheme, useThemes } from "../data/themes";
import { Button, ErrorNote } from "../ui";
import "./ThemesProposed.css";

/**
 * "Proposed themes": the groups the daemon found above the owner's notes and lessons, waiting for a
 * yes or a no. The `#id` keeps two themes with the same name apart. Nothing renders when none wait.
 */
export function ThemesProposed() {
  const themes = useThemes("proposed");
  const accept = useAcceptTheme();
  const refuse = useRefuseTheme();
  const rows = themes.data ?? [];
  if (themes.isError && themes.data === undefined) {
    return (
      <ErrorNote onRetry={() => void themes.refetch()}>
        the {DAEMON_NAME} is not responding: the proposed themes are not known
      </ErrorNote>
    );
  }
  if (rows.length === 0) return null;
  const busy = accept.isPending || refuse.isPending;
  return (
    <section className="brain-sec themes-proposed" aria-label="Proposed themes">
      <h4>Proposed themes</h4>
      <ul className="themes-list" aria-label="Proposed themes">
        {rows.map((theme) => (
          <li key={theme.id}>
            <span className="themes-head">
              <span>{theme.name}</span>
              <span className="themes-id">#{theme.id}</span>
              <span className="themes-count">
                {theme.members} {theme.members === 1 ? "member" : "members"}
              </span>
            </span>
            <span className="themes-act">
              <Button
                variant="quiet"
                aria-label={`Accept theme ${theme.id}`}
                disabled={busy}
                onClick={() => accept.mutate({ id: theme.id, name: undefined })}
              >
                Accept
              </Button>
              <Button
                variant="quiet"
                aria-label={`Refuse theme ${theme.id}`}
                disabled={busy}
                onClick={() => refuse.mutate(theme.id)}
              >
                Refuse
              </Button>
            </span>
          </li>
        ))}
      </ul>
      {(accept.isError || refuse.isError) && (
        <ErrorNote>that did not go through: the theme may already have been decided</ErrorNote>
      )}
    </section>
  );
}
