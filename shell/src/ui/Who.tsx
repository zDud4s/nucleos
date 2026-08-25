/**
 * A specialist, as a chip.
 *
 * `team_members` is `(team_id, agent_id)` with no uniqueness across teams
 * (`core/src/team.rs:434`), so one specialist can serve several departments at
 * once — and until this component existed the app never said so anywhere. That
 * is the fact `shared` carries.
 *
 * Two marks, both of them structural rather than decorative:
 *
 * - **leads** — an accent ring. The director is the one member whose absence
 *   stops a task from starting at all, so it is worth telling apart at a
 *   glance. The accent is the app's brand colour and is not a state, which is
 *   exactly right here: leading is not a status, it is an identity.
 * - **shared** — a dashed outline. It reads as "this one is not only ours",
 *   which is what a dashed border means everywhere else in this design.
 *
 * Neither is colour-only: both change the shape of the outline, and the title
 * says in words what the ring and the dashes say in form.
 */

export interface WhoProps {
  /** The agent id. Shown as-is — it is what the daemon knows the specialist by. */
  id: string;
  /** This department's director. */
  leads?: boolean;
  /** Serves at least one other department too. */
  shared?: boolean;
  /**
   * A short suffix — where else they serve, or that they were just hired.
   *
   * Kept as free text rather than an enum because the two callers want
   * different sentences and neither is a state the daemon reports.
   */
  note?: string;
}

export function Who({ id, leads, shared, note }: WhoProps) {
  const classes = ["ui-who"];
  if (leads === true) classes.push("ui-who-leads");
  if (shared === true) classes.push("ui-who-shared");

  const said = [leads === true ? "directs this department" : null, shared === true ? "serves more than one" : null]
    .filter((part) => part !== null)
    .join(", ");

  return (
    <span className={classes.join(" ")} title={said === "" ? undefined : said}>
      <span className="ui-who-id">{id}</span>
      {note === undefined ? null : <span className="ui-who-note">{note}</span>}
      {said === "" ? null : <span className="ui-who-said">{said}</span>}
    </span>
  );
}
