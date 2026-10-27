import { ApiRefusal } from "../data/client";
import { DAEMON_NAME } from "../copy";
import { useSynthesiseTheme, useTheme, useThemes } from "../data/themes";
import { Button, ErrorNote, Quiet, Skeleton } from "../ui";
import "./ThemesProposed.css";

/** A refused synthesis in the owner's words; anything else means the daemon did not answer. */
function synthesisFailure(error: unknown): string {
  if (error instanceof ApiRefusal) {
    if (error.status === 429) return "A synthesis was made less than an hour ago.";
    if (error.status === 503) return "The distiller model cannot be served right now.";
    if (error.status === 409) return "Only an accepted theme has a synthesis.";
  }
  return `the ${DAEMON_NAME} is not responding: the synthesis was not made`;
}

function AcceptedTheme({ id, name }: { id: number; name: string }) {
  const detail = useTheme(id);
  const force = useSynthesiseTheme();
  const synthesis = detail.data?.synthesis ?? null;
  return (
    <li>
      <span className="themes-head">
        <span>{name}</span>
        <span className="themes-id">#{id}</span>
      </span>
      {detail.data === undefined ? (
        detail.isError ? (
          <ErrorNote>the synthesis is not known</ErrorNote>
        ) : (
          <Skeleton label="Loading the synthesis" lines={2} />
        )
      ) : synthesis === null || synthesis === "" ? (
        <Quiet compact says="No synthesis yet: it is made after a theme is accepted, and weekly." />
      ) : (
        <p className="themes-synthesis">{synthesis}</p>
      )}
      <span className="themes-act">
        <Button
          variant="quiet"
          aria-label={`Synthesise theme ${id} now`}
          disabled={force.isPending}
          onClick={() => force.mutate(id)}
        >
          {force.isPending ? "Synthesising…" : "Synthesise now"}
        </Button>
      </span>
      {force.isError && <ErrorNote>{synthesisFailure(force.error)}</ErrorNote>}
    </li>
  );
}

/** Accepted themes with their synthesis, and a button to force a new one (at most once an hour). */
export function ThemesAccepted() {
  const themes = useThemes("accepted");
  const rows = themes.data ?? [];
  if (rows.length === 0) return null;
  return (
    <section className="brain-sec themes-accepted" aria-label="Themes">
      <h4>Themes</h4>
      <ul className="themes-list" aria-label="Accepted themes">
        {rows.map((theme) => (
          <AcceptedTheme key={theme.id} id={theme.id} name={theme.name} />
        ))}
      </ul>
    </section>
  );
}
