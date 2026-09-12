// §spec mapa-do-projeto
import { useMemo } from "react";
import type { ForeignFile, MapImport, MapModule, Seam } from "../data/project-map";
import { buildSides, isBlindSpot } from "./map-sides";

/**
 * What this reading could not see, above every view of the map.
 *
 * It used to be the picture's own header, and the picture is now one of five things this mode
 * draws over the same answer — so a header that stayed with it would be a seam behind a click for
 * the other four. §16.5 is written against exactly that: *descending may hide detail; it may never
 * hide a seam*. The junction, the stamps and the triage are all counted over this same partial
 * reading, and 190 files nothing here can read is as much a caveat on those numbers as it is on
 * the drawing.
 *
 * Building `sides` here rather than taking them as a prop is the same rule as the seam's: this
 * component is the one thing that reads them, so the derivation stays where it is used and no
 * caller can hand it a `Sides` built from a different answer than the `Seam` beside it.
 */

/**
 * §16.4's L0 — the sides of the product, and the boundary that runs between them.
 *
 * **A header rather than a step you click into**, and that is §16.5's rule applied to the
 * navigation itself. *Descending may hide detail; it may never hide a seam* — so the seam is on
 * screen whichever view is open, instead of behind a click somebody may never make.
 *
 * **Two facts sit here that look like one and are not.** *No import crosses between the sides* is
 * about source files and is trivially true — a Rust file cannot import a TypeScript module, and the
 * núcleo resolves an import only inside a file's own folder. Drawn alone it reads as independence,
 * which nobody measured. *These are the routes one side serves and the other calls* is the boundary
 * itself, read from both ends by `map_seam`, and it is the one that can be wrong: a screen asking
 * for a route nobody serves compiles, ships, and fails in front of whoever opened it.
 *
 * So the panel says both, in that order, and never lets the first stand in for the second.
 */
export interface BoundaryProps {
  modules: MapModule[];
  imports: MapImport[];
  /** What the walk found and no reader here understood. */
  unread: string[];
  foreign: ForeignFile[];
  /**
   * The boundary itself, read by the núcleo from both ends, or `undefined` from a daemon too old
   * to have read it.
   *
   * Comes off `GET /map` beside the structure rather than from a query of its own, so the sides
   * and the seam are one reading of one project. Two requests could answer about a folder that
   * moved between them, and the disagreement would show up as routes that stopped existing.
   */
  seam: Seam | undefined;
}

export function Boundary({ modules, imports, unread, foreign, seam }: BoundaryProps) {
  const sides = useMemo(
    () => buildSides(modules, imports, unread, foreign),
    [modules, imports, unread, foreign],
  );
  // Two reasons a folder earns a line, and they are different facts. A folder with no box is a
  // piece of the product this map is blind to. A folder that has a box and still holds files
  // nothing reads is a side with something behind it — `core/`'s 131 are its SQL migrations, and 15
  // of them name a section. Only the repository root is left out when it cites nothing: five config
  // files are not a side of anything, and calling them one next to `sidecars/` flattens the
  // difference this level exists to draw.
  const quiet = sides.unread.filter(
    (folder) =>
      folder.citing > 0 || (isBlindSpot(sides, folder.folder) && folder.folder !== "(root)"),
  );
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        {sides.sides.map((side) => (
          <div
            key={side.key}
            className="flex min-w-[150px] flex-col gap-0.5 rounded-lg border border-border bg-surface px-3 py-2"
          >
            <span className="font-display text-sm text-text">{side.folder}</span>
            <span className="text-xs uppercase tracking-wide text-text-faint">{side.reader}</span>
            {/* No margin of its own: the card is a column with `gap-0.5`, and a top
                margin here made one of its four gaps three times the other three.
                Invisible until round 11 layered the reset that was cancelling it. */}
            <span className="text-xs text-text-muted">
              <span className="font-display text-sm text-text">{side.files}</span> file
              {side.files === 1 ? "" : "s"} · {side.imports} import{side.imports === 1 ? "" : "s"}{" "}
              inside
            </span>
            <span
              className="text-xs text-text-muted"
              title="Of the files naming a section, the ones whose header says which document it belongs to. Counted over citing files, as the header above is, so the two agree."
            >
              {side.citing === 0
                ? "nothing here cites a section"
                : `${side.declared} of ${side.citing} say which document`}
            </span>
            <span className="text-xs text-text-muted" title="Files with a test beside them.">
              {side.tested} have a test
            </span>
          </div>
        ))}
      </div>
      <p className="max-w-prose text-sm text-text-muted">
        {sides.crossing === 0 ? (
          <>
            <span className="text-text">No import crosses between them, and none could</span> — a
            Rust file cannot import a TypeScript module, and the núcleo resolves an import only
            inside the file&apos;s own folder. So this says these sides share no source; it does not
            say they are independent.
          </>
        ) : (
          <>
            <span className="ui-wrong">
              {sides.crossing} import{sides.crossing === 1 ? "" : "s"} cross between sides.
            </span>{" "}
            Nothing here should be able to do that, so the walk and this drawing disagree about
            what a side is.
          </>
        )}
        {sides.loose > 0 ? (
          <>
            {" "}
            <span className="ui-wrong">
              {sides.loose} import{sides.loose === 1 ? " ends" : "s end"} on no file this map lists.
            </span>
          </>
        ) : null}
      </p>
      {seam === undefined ? (
        <p className="max-w-prose text-sm text-text-faint">
          This daemon does not report the boundary, so what runs between these sides is unread here.
        </p>
      ) : (
        <Routes seam={seam} />
      )}
      {quiet.map((folder) => (
        <p key={folder.folder} className="max-w-prose text-sm text-text-muted">
          <span className="font-mono text-text">{folder.folder}/</span>{" "}
          {isBlindSpot(sides, folder.folder) ? (
            <>
              is <span className="text-text">{folder.files} files nothing here can read</span> — no
              box above is about it.
            </>
          ) : (
            <>
              also holds <span className="text-text">{folder.files} files nothing here can read</span>
              , behind the box that is about it.
            </>
          )}{" "}
          {folder.citing === 0
            ? "None of them names a section, so nothing can be said about what they were asked to do."
            : `${folder.citing} of them ${folder.citing === 1 ? "names" : "name"} a section, and ${folder.declared} ${folder.declared === 1 ? "says" : "say"} which document.`}
        </p>
      ))}
    </div>
  );
}

/**
 * What actually runs between the sides: the routes, counted from both ends.
 *
 * **The unmatched list is the finding and everything else is context for it**, so it is the only
 * thing here drawn in the colour that means *look*. The two lists that flank it are honest about
 * being weaker: a *computed* call is one the núcleo could not finish reading, and a route no screen
 * calls is not a route nothing calls — agents and sidecars reach this daemon over the same HTTP,
 * which nothing here reads. Printing the size of the blind spot next to the list it corrupts is
 * §16.5 one more time: a level may hide detail and may never hide a seam.
 */
function Routes({ seam }: { seam: Seam }) {
  // **A false green is the one thing this panel must never show.** With no route found, nothing can
  // fail to match, `unmatched` is empty, and the branch below would announce that nothing asks for a
  // route that does not exist — about a project whose daemon this map simply cannot read. The
  // núcleo already refuses to compare against an empty list; this says so where somebody can see it.
  if (seam.served.length === 0) {
    return (
      <p className="max-w-prose text-sm text-text-faint">
        Nothing here registers a route this map knows how to read, so the boundary could not be read
        from the daemon&apos;s side.
        {seam.calls > 0
          ? ` The ${seam.calls} calls the shell makes are left uncompared: a call cannot miss a route in a project where no route was found.`
          : ""}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      <p className="max-w-prose text-sm text-text-muted">
        What passes between them is HTTP, and the boundary is{" "}
        <span className="font-display text-sm text-text">{seam.served.length}</span> routes. The
        shell makes {seam.calls} calls to them and{" "}
        <span className="text-text">{seam.matched}</span> match a route exactly.
      </p>
      {seam.unmatched.length > 0 ? (
        <div className="rounded-lg border border-tone-danger-border bg-tone-danger-bg px-3 py-2">
          <p className="text-sm text-text">
            {seam.unmatched.length} call{seam.unmatched.length === 1 ? "" : "s"} ask for a route this
            daemon does not serve.
          </p>
          <ul className="mt-1 flex flex-col gap-0.5 font-mono text-[11px] text-text-muted">
            {seam.unmatched.map((call) => (
              <li key={`${call.file}:${call.line}:${call.path}`}>
                {call.path} — {call.file}:{call.line}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="max-w-prose text-sm text-text-muted">
          <span className="text-text">Nothing asks for a route that does not exist.</span> That is a
          measurement and not a property: it is checked every time this is read, and the day
          somebody renames a route without renaming its caller it stops being true here first.
        </p>
      )}
      <p className="max-w-prose text-xs text-text-faint">
        {seam.computed.length > 0 ? (
          <>
            {seam.computed.length} call{seam.computed.length === 1 ? "" : "s"} build part of the path
            at run time, so what they reach is a candidate and not a fact
            <span title={seam.computed.map((call) => `${call.path} — ${call.file}:${call.line}`).join("\n")}>
              {" "}
              ⓘ
            </span>
            .{" "}
          </>
        ) : null}
        {seam.opaque.length > 0 ? (
          <>
            {seam.opaque.length} hand the path in from elsewhere and could not be read at all
            <span title={seam.opaque.map((site) => `${site.file}:${site.line}`).join("\n")}> ⓘ</span>,
            which is why the next number is a ceiling rather than a count.{" "}
          </>
        ) : null}
        {seam.uncalled.length} route{seam.uncalled.length === 1 ? "" : "s"} no screen calls
        <span title={seam.uncalled.join("\n")}> ⓘ</span> — agents and sidecars reach this daemon over
        the same HTTP, which this map does not read, so that is not a list of dead code.
      </p>
    </div>
  );
}
