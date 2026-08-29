// §spec mapa-do-projeto
import { useProjectMap } from "../data/project-map";
import { MapaCanvas } from "../canvas/MapaCanvas";
import { buildMap, declaredCoverage } from "../canvas/map-model";
import { Carimbos } from "./Carimbos";
import { ExtrairSpec } from "./ExtrairSpec";
import { Juncao } from "./Juncao";
import { MapaPorAprovar } from "./MapaPorAprovar";
import { Triagem } from "./Triagem";

/**
 * "What is in here, what did nobody ask for, and what did this project actually decide?"
 *
 * Two of the three layers are here, the join between them, and the two axes read over it: the
 * structure, derived off disk and always true; the intention — a model reading one spec and
 * proposing the decisions it fixes, which nothing enters the map without the owner answering line
 * by line; the junction, which is decision 3 of the spec and the reason the other two are worth
 * deriving; the stamps, which are what the owner said about each line and whether it is still true;
 * and the triage, which is what a model thought was worth their eyes. The evidence layer is not
 * here, and the mode says so in words rather than showing a map that looks complete and is not. A
 * surface that implies it has layers it does not have is the false confidence again, with better
 * pixels.
 *
 * **The junction, the stamps and the triage are three axes and never one reading.** §5 refuses to
 * flatten them — *"achatá-las numa só punha o triador e o dono a falar pela mesma boca"* — so they
 * are three panels with three headings, and no number on this screen has already decided how they
 * combine. §6.1 is the sharpest edge of that rule and it is kept by rendering rather than promised:
 * a silence is a claim about the triager, a green is the owner's, and nothing here draws the two in
 * a way that could be mistaken one for the other.
 *
 * **The three panels below read three different questions, and one failing does not silence the
 * others.** The structure is derived by walking the project's folder; the pile is a table the
 * daemon answers without touching disk at all — `get_project_map_decisions` resolves the row and
 * never the folder, deliberately, "so a project whose folder has moved still has a pile, which is
 * exactly what somebody looking at a broken project wants". A mode that returned a single sentence
 * on the first failure would take that away for no reason.
 *
 * **The junction is not a fourth question, which is why it is not a fourth panel.** It comes back
 * on the same answer as the structure — `GET /projects/{id}/map` returns both, flattened — so it
 * is drawn inside the component that holds that query. Giving it a query of its own would ask the
 * daemon to walk the same tree twice per open, and would put two answers to one question on one
 * screen, free to disagree about a project whose folder moved between them.
 */

export interface ModeMapaProps {
  projectId: string;
}

export function ModeMapa({ projectId }: ModeMapaProps) {
  return (
    <div className="flex flex-col gap-8">
      <Derived projectId={projectId} />
      <ExtrairSpec projectId={projectId} />
      <MapaPorAprovar projectId={projectId} />
      <p className="max-w-prose text-sm text-text-muted">
        Structure, intention, the join between them, your verdict on each line, and what a model
        thought was worth your eyes. The evidence layer is a slice that does not exist yet — nothing
        here reads a test, or a gate, or asks whether the code that claims a decision actually does
        what it says.
      </p>
    </div>
  );
}

/**
 * Everything the one map query answers: the structure it walked, and the junction over it.
 *
 * Named for the query and not for a layer, because it now draws two things and a component called
 * `Structure` that renders the junction would be lying about itself in the file that argues
 * hardest against exactly that.
 *
 * **The headline here reports what this half alone knows, and nothing else.** It used to report
 * `modules.filter(m => !m.declares).length` under the words "declaring nothing they implement",
 * which is the same concept as `junction.counts.unclaimed` — and the two disagreed. `declares` is
 * `source.contains('§')`, the file's own gesture at a section, bare `§` included; `unclaimed` is
 * an empty `cites`, and a TypeScript module's `cites` folds in its sibling test's, the way a Rust
 * module has always got its `#[cfg(test)]` citations for free. Four modules of this repository are
 * `declares: false` with a non-empty `cites` — their tests name what they prove, so they are not
 * code nobody asked for. Two numbers meaning almost the same thing and disagreeing by four, on one
 * screen, is precisely the confusion this mode exists to remove, so the junction is now the single
 * owner of that count and this reports the size of what it could read.
 */
function Derived({ projectId }: { projectId: string }) {
  const map = useProjectMap(projectId);

  if (map.isError) {
    return (
      <p className="text-sm text-text-faint">
        The núcleo could not read this project&rsquo;s map — its folder may have moved.
      </p>
    );
  }
  if (map.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the project&rsquo;s tree…</p>;
  }

  const {
    modules,
    imports,
    unread,
    foreign,
    junction,
    standings,
    stamps,
    triage,
    triage_counts,
    git_would_not_answer,
    recency,
    last_triaged_at,
  } = map.data;
  // `buildMap` rather than `imports.length`, and the difference is the whole point: this counts
  // the links the map would actually draw, which drops any edge with an end it cannot find. The
  // núcleo sends none of those today, and the day it does this number must not quietly start
  // counting things nobody will ever see.
  const built = buildMap(modules, imports);
  // **§8 on screen, because until now it was legible only to the parser.** A bare `§7` names a
  // section of *some* document; the header says which. Every confirmation this mode draws below
  // rests on that, so a reader has to be able to ask how much of the project has said it — a
  // green over an undeclared file is a guess wearing the same colour as a fact.
  //
  // Counted over the files that name a section and not over `modules.length`, and the two must
  // not be confused: a file with no `§` has nothing to declare, and the denominator on the line
  // above is a different question with a different answer.
  const declared = declaredCoverage(modules, foreign);

  return (
    <div className="flex flex-col gap-6">
      <div>
        <p className="mt-1 font-display text-3xl text-text">
          {modules.length}
          <span className="ml-2 text-sm text-text-faint">
            module{modules.length === 1 ? "" : "s"} this reader could read
          </span>
        </p>
        {declared.citing > 0 ? (
          <p className="mt-2 font-display text-3xl text-text">
            {declared.saying}
            <span className="ml-2 text-sm text-text-faint">
              of {declared.citing} file{declared.citing === 1 ? "" : "s"} naming a section say
              which document it belongs to
            </span>
          </p>
        ) : null}
        <p className="mt-1 text-xs text-text-muted">
          joined by {built.edges.length} link{built.edges.length === 1 ? "" : "s"}
          {unread.length > 0
            ? ` · ${unread.length} file${unread.length === 1 ? "" : "s"} in a language it cannot read yet`
            : ""}
        </p>
      </div>
      {/*
        The structure layer as a picture, one level above the file. Drawn here and not as a panel
        of its own for the reason the junction is: it reads the same answer, and a query of its own
        would walk the tree twice per open.

        **Above the junction, and the order is an argument.** The junction says how firmly each
        decision is tied to code; whether that means anything depends on how much of the project
        declared which document its sections belong to. The picture is where that is visible, so it
        is read first.
      */}
      <MapaCanvas modules={modules} foreign={foreign} imports={imports} />
      <Juncao junction={junction} projectId={projectId} />
      {/*
        Drawn here rather than as a panel of its own for the reason the junction is: the standings,
        the header and `git_would_not_answer` come back on this same answer, flattened. A query of
        its own would ask the daemon to walk a thousand-file tree twice per open, and would put two
        readings of one question on one screen, free to disagree about a project whose folder moved
        between them.
      */}
      <Carimbos
        projectId={projectId}
        junction={junction}
        standings={standings}
        stamps={stamps}
        triage={triage}
        triageCounts={triage_counts}
        gitWouldNotAnswer={git_would_not_answer}
      />
      {/*
        The third axis, drawn off this same answer for the reason the other two are — the
        judgements, their tally and §10's ordering all arrive flattened onto `GET /map`, and a
        query of its own would walk the tree twice per open and put two readings of one question on
        one screen. It holds one query the map cannot serve: §6.2's silenced pile has a door of its
        own, because *sempre acessível* has to survive this map failing to read a folder.

        **Below the stamps, and the order is an argument.** §5.3's header sits at the top of the
        stamp panel and is the first thing read; the triage panel is what explains two of its four
        numbers — why `J` is larger than the lapsed pile and why silencing did not make `K`
        smaller — so it reads as the answer to a question the header has just raised.
      */}
      <Triagem
        projectId={projectId}
        junction={junction}
        triage={triage}
        counts={triage_counts}
        recency={recency}
        lastTriagedAt={last_triaged_at}
      />
    </div>
  );
}
