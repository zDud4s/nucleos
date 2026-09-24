// §spec mapa-do-projeto
import { useMemo, type ReactNode } from "react";
import type { ForeignFile, MapImport, MapModule, Seam } from "../data/project-map";
import { buildSides, isBlindSpot, type Sides, type Unread } from "./map-sides";
import { Button, Quiet, Row, Rows } from "../ui";

/**
 * What this reading found, and what it could not see — above every view of the map.
 *
 * It used to be the picture's own header, and the picture is now one of five things this mode
 * draws over the same answer — so a header that stayed with it would be a seam behind a click for
 * the other four. §16.5 is written against exactly that: *descending may hide detail; it may never
 * hide a seam*. The junction, the stamps and the triage are all counted over this same partial
 * reading, and 190 files nothing here can read is as much a caveat on those numbers as it is on
 * the drawing.
 *
 * **It answers "is everything fine?" first, and in one line.** The first version of this header
 * led with the size of the project and three paragraphs of explanation, and the exceptions — the
 * imports that cross sides, the files nothing reads, the calls that miss — sat inside the prose at
 * the same weight as the caveats around them. The owner opens this to verify, and had to read an
 * essay to find three numbers. So: a verdict, then one counted row per exception, each opening its
 * own proof, and the reasoning one click away rather than on every open. Nothing was cut; it moved
 * to where it is asked for.
 *
 * Building `sides` here rather than taking them as a prop is the same rule as the seam's: this
 * component is the one thing that reads them, so the derivation stays where it is used and no
 * caller can hand it a `Sides` built from a different answer than the `Seam` beside it.
 */

/**
 * §16.4's L0 — the sides of the product, and the boundary that runs between them.
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
  /**
   * The matrix's own count of what points forwards and backwards, when the caller has built it.
   *
   * Taken from the matrix and never recounted here, so the row that announces it and the diagonal
   * that draws it are one number.
   */
  direction?: { forward: number; back: number };
  /** Opens the drawing that shows the backwards dependencies, when there is somewhere to go. */
  onMatrix?: () => void;
  /** The line under the verdict: how big this reading is and when it was taken. */
  meta?: ReactNode;
}

/** One exception this reading found, as a counted row. */
interface Finding {
  key: string;
  /**
   * `wrong` is a tripwire that fired — something that must be zero and is not. `limit` is a part
   * of the project this reading could not see. `measure` is a number worth watching that nothing
   * says is wrong. Only the first counts towards the verdict: a blind spot is a caveat on the
   * answer, and folding it into the count would cry wolf on every open.
   */
  weight: "wrong" | "limit" | "measure";
  line: ReactNode;
  /** What opens under the row. Absent for a row whose line is the whole of it. */
  detail?: ReactNode;
  /** A way to the drawing that shows it, beside the line. */
  action?: ReactNode;
}

const plural = (count: number, one: string, many: string) => (count === 1 ? one : many);

export function Boundary({
  modules,
  imports,
  unread,
  foreign,
  seam,
  direction,
  onMatrix,
  meta,
}: BoundaryProps) {
  const sides = useMemo(
    () => buildSides(modules, imports, unread, foreign),
    [modules, imports, unread, foreign],
  );
  const findings = collect(sides, seam, direction, onMatrix);
  const wrong = findings.filter((finding) => finding.weight === "wrong").length;
  const limits = findings.filter((finding) => finding.weight === "limit").length;

  return (
    <section aria-label="What this reading found" className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        {/*
          The verdict. It counts tripwires only — a number that must be zero and is not — and says
          "checks" rather than "is fine": this reading measures a handful of things, and a line
          claiming the project is healthy would be a green nobody earned.
        */}
        <p className="m-0 font-display text-lg font-semibold text-text">
          {wrong > 0 ? (
            <span className="ui-wrong">
              {wrong} {plural(wrong, "finding", "findings")} on this reading
            </span>
          ) : (
            "Nothing this reading checks came back wrong"
          )}
          {limits > 0 ? (
            <span className="font-normal text-text-muted">
              {" "}
              · {limits} {plural(limits, "limit", "limits")} on what it could see
            </span>
          ) : null}
        </p>
        {meta}
      </div>

      {findings.length === 0 ? null : (
        <Rows label="Exceptions on this reading">
          {findings.map((finding) => (
            <Row key={finding.key} layout="line" dense>
              {finding.detail === undefined ? (
                <span className="min-w-0 flex-1 text-sm">{finding.line}</span>
              ) : (
                <details className="min-w-0 flex-1 text-sm">
                  <summary className="cursor-pointer">{finding.line}</summary>
                  <div className="mt-2 flex max-w-prose flex-col gap-2 text-xs text-text-muted">
                    {finding.detail}
                  </div>
                </details>
              )}
              {finding.action}
            </Row>
          ))}
        </Rows>
      )}

      <SidesLine sides={sides} seam={seam} />
    </section>
  );
}

/** Every exception, in the order a reader should meet them: the faults, then the blind spots. */
function collect(
  sides: Sides,
  seam: Seam | undefined,
  direction: BoundaryProps["direction"],
  onMatrix: BoundaryProps["onMatrix"],
): Finding[] {
  const found: Finding[] = [];

  if (sides.crossing > 0) {
    found.push({
      key: "crossing",
      weight: "wrong",
      line: (
        <span className="ui-wrong">
          {sides.crossing} {plural(sides.crossing, "import crosses", "imports cross")} between sides
        </span>
      ),
      detail: (
        <>
          <p className="m-0">
            Nothing should be able to: a Rust file cannot import a TypeScript module, and the
            núcleo resolves an import only inside the file&apos;s own folder. So the map&apos;s rule
            for what a side is is wrong for these files, not your code — report it with this list.
          </p>
          <ImportList lines={sides.crossings} label="Imports that cross between sides" />
        </>
      ),
    });
  }

  if (sides.loose > 0) {
    found.push({
      key: "loose",
      weight: "wrong",
      line: (
        <span className="ui-wrong">
          {sides.loose} {plural(sides.loose, "import ends", "imports end")} on no file this map
          lists
        </span>
      ),
      detail: (
        <>
          <p className="m-0">
            The núcleo only sends imports between files it listed, so this says its two halves
            have drifted. Report it with this list.
          </p>
          <ImportList lines={sides.strays} label="Imports with an end on no listed file" />
        </>
      ),
    });
  }

  if (seam !== undefined && seam.served.length > 0 && seam.unmatched.length > 0) {
    found.push({
      key: "unmatched",
      weight: "wrong",
      line: (
        <span className="ui-wrong">
          {seam.unmatched.length} {plural(seam.unmatched.length, "call", "calls")} ask for a route
          this daemon does not serve
        </span>
      ),
      detail: (
        <>
          <p className="m-0">
            A screen asking for a route nobody serves compiles, ships, and fails in front of
            whoever opens it. Each line is the path it asks for and where it asks.
          </p>
          <ul
            aria-label="Calls to a route nobody serves"
            className="m-0 flex list-none flex-col gap-0.5 p-0 font-mono text-[11px] text-text-muted"
          >
            {seam.unmatched.map((call) => (
              <li key={`${call.file}:${call.line}:${call.path}`}>
                {call.path} — {call.file}:{call.line}
              </li>
            ))}
          </ul>
        </>
      ),
    });
  }

  if (direction !== undefined && direction.back > 0) {
    const total = direction.back + direction.forward;
    const share = Math.round((100 * direction.back) / total);
    found.push({
      key: "back",
      weight: "measure",
      line: (
        <span className="text-text-muted">
          <span className="font-mono tabular-nums text-text">{direction.back}</span> of {total}{" "}
          dependencies between communities point backwards ·{" "}
          <span className="font-mono tabular-nums">{share}%</span>
        </span>
      ),
      action:
        onMatrix === undefined ? undefined : (
          <Button variant="quiet" onClick={onMatrix}>
            show them
          </Button>
        ),
    });
  }

  // Two reasons a folder earns a row, and they are different facts. A folder with no box is a
  // piece of the product this map is blind to. A folder that has a box and still holds files
  // nothing reads is a side with something behind it — `core/`'s 131 are its SQL migrations, and 15
  // of them name a section. Only the repository root is left out when it cites nothing: five config
  // files are not a side of anything, and calling them one next to `sidecars/` flattens the
  // difference this level exists to draw.
  const quiet = sides.unread.filter(
    (folder) =>
      folder.citing > 0 || (isBlindSpot(sides, folder.folder) && folder.folder !== "(root)"),
  );
  for (const folder of quiet) {
    found.push({
      key: `unread:${folder.folder}`,
      weight: "limit",
      line: <UnreadLine sides={sides} folder={folder} />,
      detail: (
        <p className="m-0">
          {isBlindSpot(sides, folder.folder)
            ? "No side of the map is about this folder. "
            : "They sit behind the side that is about this folder. "}
          {folder.citing === 0
            ? "None of them names a section, so nothing can be said about what they were asked to do."
            : `${folder.citing} of them ${plural(folder.citing, "names", "name")} a section, and ${folder.declared} ${plural(folder.declared, "says", "say")} which document.`}
        </p>
      ),
    });
  }

  if (seam === undefined) {
    found.push({
      key: "seam",
      weight: "limit",
      line: (
        <span className="text-text-muted">
          This daemon does not report the boundary, so what runs between these sides is unread
          here.
        </span>
      ),
    });
  } else if (seam.served.length === 0) {
    // **A false green is the one thing this panel must never show.** With no route found, nothing
    // can fail to match, `unmatched` is empty, and the line below the sides would announce that
    // nothing asks for a route that does not exist — about a project whose daemon this map simply
    // cannot read. The núcleo already refuses to compare against an empty list; this says so where
    // somebody can see it.
    found.push({
      key: "seam",
      weight: "limit",
      line: (
        <span className="text-text-muted">
          Nothing here registers a route this map knows how to read, so the boundary could not be
          read from the daemon&apos;s side.
        </span>
      ),
      detail:
        seam.calls > 0 ? (
          <p className="m-0">
            The {seam.calls} calls the shell makes are left uncompared: a call cannot miss a route
            in a project where no route was found.
          </p>
        ) : undefined,
    });
  }

  return found;
}

/** A folder nothing here can read, in one line: which, and how many. */
function UnreadLine({ sides, folder }: { sides: Sides; folder: Unread }) {
  return (
    <span className="text-text-muted">
      <span className="font-mono text-text">{folder.folder}/</span>{" "}
      {isBlindSpot(sides, folder.folder) ? "is " : "also holds "}
      <span className="text-text">{folder.files} files nothing here can read</span>
    </span>
  );
}

/**
 * A list of imports as `from → to`, which is the whole of what an import is on this answer.
 *
 * No line number, and not by omission: the núcleo sends an import as two paths, so `from` is the
 * file to open. Capped like every pile on this map, with the cut counted out loud.
 */
function ImportList({ lines, label }: { lines: MapImport[]; label: string }) {
  const shown = lines.slice(0, 40);
  return (
    <>
      <ul
        aria-label={label}
        className="m-0 flex list-none flex-col gap-0.5 p-0 font-mono text-[11px] text-text-muted"
      >
        {shown.map((line) => (
          <li key={`${line.from}>${line.to}`}>
            {line.from} → {line.to}
          </li>
        ))}
      </ul>
      {lines.length > shown.length ? (
        <p className="m-0">{lines.length - shown.length} more not shown here.</p>
      ) : null}
    </>
  );
}

/**
 * The sides, one short line each, and what runs between them.
 *
 * They were four-line cards the width of a column, the second-largest thing on the page, carrying
 * context rather than an answer. A line apiece keeps every number and gives the space back to the
 * rows above, which are the answer.
 */
function SidesLine({ sides, seam }: { sides: Sides; seam: Seam | undefined }) {
  const served = seam !== undefined && seam.served.length > 0 ? seam : null;
  const says = [
    sides.crossing === 0 && sides.sides.length > 1 ? "These sides share no source." : null,
    served === null
      ? null
      : `What passes between them is HTTP: ${served.served.length} ${plural(served.served.length, "route", "routes")}, and ${served.matched} of the shell's ${served.calls} ${plural(served.calls, "call", "calls")} match one exactly.` +
        (served.unmatched.length === 0 ? " Nothing asks for a route that does not exist." : ""),
  ]
    .filter((part) => part !== null)
    .join(" ");

  return (
    <div className="flex flex-col gap-1">
      <ul
        aria-label="The sides of this project"
        className="m-0 flex list-none flex-wrap gap-x-6 gap-y-1 p-0 text-xs text-text-muted"
      >
        {sides.sides.map((side) => (
          <li key={side.key}>
            <span className="font-display text-sm text-text">{side.folder}</span>{" "}
            <span className="text-text-faint">{side.reader}</span> · {side.files}{" "}
            {plural(side.files, "file", "files")} · {side.imports}{" "}
            {plural(side.imports, "import", "imports")} inside · {side.tested} tested ·{" "}
            <span title="Of the files naming a section, the ones whose header says which document it belongs to.">
              {side.citing === 0
                ? "nothing here cites a section"
                : `${side.declared} of ${side.citing} say which document`}
            </span>
          </li>
        ))}
      </ul>
      {says === "" ? null : (
        <Quiet says={says}>
          <SeamWhy sides={sides} seam={served} />
        </Quiet>
      )}
    </div>
  );
}

/**
 * The reasoning behind the line under the sides, asked for rather than shown on every open.
 *
 * The three lists that used to hide in `title` tooltips — calls whose path is built at run time,
 * calls handing the path in from elsewhere, routes no screen calls — are here as lists, because a
 * hover is not something a keyboard or a screen reader can do.
 */
function SeamWhy({ sides, seam }: { sides: Sides; seam: Seam | null }) {
  return (
    <>
      {sides.crossing === 0 && sides.sides.length > 1 ? (
        <p className="m-0">
          No import crosses between them, and none could — a Rust file cannot import a TypeScript
          module, and the núcleo resolves an import only inside the file&apos;s own folder. So this
          says these sides share no source; it does not say they are independent.
        </p>
      ) : null}
      {seam === null ? null : (
        <>
          <p className="m-0">
            Matching is a measurement and not a property: it is checked every time this is read,
            and the day somebody renames a route without renaming its caller it stops being true
            here first.
          </p>
          {seam.computed.length > 0 ? (
            <SiteList
              intro={`${seam.computed.length} ${plural(seam.computed.length, "call builds", "calls build")} part of the path at run time, so what they reach is a candidate and not a fact:`}
              items={seam.computed.map((call) => `${call.path} — ${call.file}:${call.line}`)}
            />
          ) : null}
          {seam.opaque.length > 0 ? (
            <SiteList
              intro={`${seam.opaque.length} hand the path in from elsewhere and could not be read at all, which is why the next number is a ceiling rather than a count:`}
              items={seam.opaque.map((site) => `${site.file}:${site.line}`)}
            />
          ) : null}
          <SiteList
            intro={`${seam.uncalled.length} ${plural(seam.uncalled.length, "route", "routes")} no screen calls — agents and sidecars reach this daemon over the same HTTP, which this map does not read, so that is not a list of dead code:`}
            items={seam.uncalled}
          />
        </>
      )}
    </>
  );
}

function SiteList({ intro, items }: { intro: string; items: string[] }) {
  return (
    <div>
      <p className="m-0">{intro}</p>
      {items.length === 0 ? null : (
        <ul className="m-0 mt-0.5 flex list-none flex-col gap-0.5 p-0 font-mono text-[11px]">
          {items.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
