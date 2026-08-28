// §spec mapa-do-projeto
import { isApiRefusal } from "../data/client";
import type { Anchored, Loss, Orphan } from "../data/project-map";
import { useOrphanCheck, useRecordAnchor } from "../data/project-map";

/**
 * §14's guard, on one row of *declared, with no code*.
 *
 * **The question this answers is not *did any file lose a citation?* It is *was this never built,
 * or did it lose the comment?*** — and that only means anything at the instant somebody is looking
 * at one line the map says nothing implements. Every anchor on this map rests on a `§N` written in
 * a comment; delete the comment and the decision joins that pile indistinguishable from one nobody
 * ever wrote code for. That is §1 of the design happening to the instrument built against §1.
 *
 * **It never runs until it is pressed** (§14.3). A query that fired on render would walk the
 * repository's history once per row of this panel, on a screen that opens all the time and asks
 * this almost never. The cost is paid by whoever is actually looking.
 *
 * **It answers a fact and never a verdict.** The núcleo says which file carried the text and in
 * which commit it stopped. Whether that means the decision was implemented is a judgement, and §5
 * gives every judgement on this map to one person — the parser cannot even tell *this implements
 * §7.1* from *as §7.1 explains*, which is why {@link Answer} says **named** everywhere and never
 * *implemented*.
 */
export function Orfa({ projectId, row }: { projectId: string; row: Anchored }) {
  const ask = useOrphanCheck(projectId);
  const answer = ask.data;

  /*
    Whether pressing again could produce a different answer, which is the ONLY thing that decides
    whether the button stays. `unreadable` says *git is there and would not answer* and the sentence
    it draws says to try again — so taking the button away with it would be advice nobody can act
    on. `no_repository` is the other half of that distinction and keeps no button, because a project
    added from outside a repository will never succeed at trying: it is `map_stamp::Watch`'s §11
    argument, arriving on the surface where it decides what somebody can press. The four real
    answers are settled facts about the history and re-asking them is noise.
  */
  const retryable = answer === undefined || answer.state === "unreadable";

  return (
    <div className="flex flex-col gap-1">
      {retryable ? (
        <div>
          <button
            type="button"
            disabled={ask.isPending}
            onClick={() => ask.mutate({ slug: row.spec_slug, section: row.section })}
            className="rounded-md border border-border px-3 py-1 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
          >
            {ask.isPending
              ? "reading the history…"
              : answer === undefined
                ? "was this ever named?"
                : "ask git again"}
          </button>
        </div>
      ) : null}
      {ask.isError ? <Failed error={ask.error} /> : null}
      {answer === undefined ? null : <Answer answer={answer} />}
      {answer?.state === "lost" ? (
        <Remember projectId={projectId} row={row} losses={answer.losses} />
      ) : null}
    </div>
  );
}

/**
 * Why the question could not be put, which is never a finding about the decision.
 *
 * **The refusals are the daemon’s and are worth spelling out separately from the answers**, for
 * the reason {@link Answer} gives about `not_in_window`: *the guard could not be asked* and *the
 * guard found nothing* are different facts, and a surface that blurred them would let a broken
 * request read as evidence that a decision has no code.
 *
 * A `422` here means the decision’s heading carries no number — which is `unnumbered`, and those
 * rows are drawn in their own pile precisely so nobody reads a search into them. It should be
 * unreachable from this button and is answered anyway rather than falling through to a generic
 * sentence, because the day it does happen the sentence is the only thing that says why.
 */
function Failed({ error }: { error: unknown }) {
  const box =
    "max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted";

  if (!isApiRefusal(error)) {
    return (
      <p className={box}>
        The núcleo did not answer, so no history was read. That says nothing about the decision
        either way.
      </p>
    );
  }

  const sentence =
    error.status === 422
      ? "This decision’s heading carries no number, so there is nothing to look for in the history — the same reason it anchors nothing."
      : error.status === 404
        ? "This project, or the document this decision came from, is not there any more."
        : error.detail;

  return <p className={box}>{sentence}</p>;
}

/**
 * One sentence per state, and the three refusals are three different sentences.
 *
 * **`not_in_window` is the one that must never be drawn like `never_named`.** *I did not find it*
 * and *I did not search all of it* are different facts, and rendering them alike would assert
 * *this was never built* about a history nobody read to the end — the silently-wrong answer this
 * whole feature exists to refuse. On a repository longer than the window it is also the ONLY
 * negative that ever arrives, so it is the sentence most people will read.
 */
function Answer({ answer }: { answer: Orphan }) {
  switch (answer.state) {
    case "never_named":
      return (
        <p className="max-w-prose text-xs text-text-muted">
          Nothing in this repository's history ever named this section. It has no code, and that
          is the whole of it — the comment was not deleted, it was never written.
        </p>
      );

    case "still_named":
      return (
        <p className="max-w-prose text-xs text-text-muted">
          Something names it right now — {answer.paths.join(" · ")} — so this line is not what it
          looks like. Worth saying out loud rather than hiding the row: the panel and the history
          disagree, and one of them is stale.
        </p>
      );

    case "lost":
      return (
        <div className="flex flex-col gap-1">
          <ul className="flex flex-col gap-0.5" aria-label="Where the citation went">
            {answer.losses.map((loss) => (
              <Went key={`${loss.path}@${loss.commit}`} loss={loss} />
            ))}
          </ul>
          <p className="max-w-prose text-xs text-text-faint">
            That is a fact about git and not a verdict: it says the text left the file, not whether
            the decision was ever implemented. Reading the commit is the only way to tell whether
            the comment went on purpose.
          </p>
        </div>
      );

    case "not_in_window":
      return (
        <p className="max-w-prose text-xs text-text-muted">
          Nothing in the last {answer.window} commits named it. There is older history this did not
          read, so this is <em>not found</em> and not <em>never named</em> — the two are different
          facts and only one of them is a claim about your code.
        </p>
      );

    case "unreadable":
      return (
        <p className="max-w-prose text-xs text-text-muted">
          Git is there and would not answer, so nothing was read. Worth trying again; nothing here
          says anything about the decision either way.
        </p>
      );

    case "no_repository":
      return (
        <p className="max-w-prose text-xs text-text-muted">
          This project has no repository, so there is no history to ask. Nothing to retry, and
          nothing here is a finding about the decision.
        </p>
      );
  }
}

/**
 * Turn what the history found into something no rewrite can delete again (§14).
 *
 * **This closes the loop, and it is the reason the guard and the record are one feature rather
 * than two.** The guard has just said which file carried the citation and in which commit it
 * stopped; writing that down by hand would mean retyping a path the screen is already showing.
 * One press, and the association stops living in a comment a model can drop.
 *
 * **The destination path and not the one that lost it**, when the commit renamed the file.
 * `path` is where the citation WAS; a record has to name a file that is there now, or the
 * núcleo refuses it and — worse, if it did not — the anchor would sit in the stamp diff as a
 * permanent `gone` for a file nobody deleted.
 *
 * It writes a fact and never a verdict. Nothing here says the decision is implemented; it says
 * these files are the ones to look at, which is what §5 leaves to one person to judge.
 */
function Remember({
  projectId,
  row,
  losses,
}: {
  projectId: string;
  row: Anchored;
  losses: Loss[];
}) {
  const record = useRecordAnchor(projectId);
  const paths = losses.map((loss) => loss.renamed_to ?? loss.path);

  if (record.isSuccess) {
    return (
      <p className="max-w-prose text-xs text-text-muted">
        Written down. {paths.join(" · ")} {plural(paths.length, "is", "are")} this decision’s code
        now, and deleting the comment again will not take that away — it will say so instead.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-1">
      <div>
        <button
          type="button"
          disabled={record.isPending}
          onClick={() => record.mutate({ decision_id: row.decision_id, paths })}
          className="rounded-md border border-border px-3 py-1 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          {record.isPending
            ? "writing it down…"
            : `write ${plural(paths.length, "this file", "these files")} down as its code`}
        </button>
      </div>
      {record.isError ? (
        <p className="max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
          {isApiRefusal(record.error) && record.error.status === 422
            ? "Those files are not in the project any more, so there is nothing to point at. A record has to name a file that is there."
            : "The núcleo did not take it, so nothing was written down."}
        </p>
      ) : null}
    </div>
  );
}

function plural(count: number, one: string, many: string): string {
  return count === 1 ? one : many;
}

/**
 * Where one file’s citation went.
 *
 * The whole commit id and never an abbreviation: this sentence exists to be pasted into
 * `git show`, and an abbreviation is a hash that stops working when the repository grows.
 */
function Went({ loss }: { loss: Loss }) {
  return (
    <li className="max-w-prose text-xs text-text-muted">
      <span className="font-mono text-text">{loss.path}</span> named it until{" "}
      <span className="font-mono">{loss.commit}</span> — “{loss.subject}”, on{" "}
      {new Date(loss.at * 1000).toLocaleDateString()}
      {loss.renamed_to === null ? null : (
        <>
          {" "}
          (that commit moved it to <span className="font-mono">{loss.renamed_to}</span>)
        </>
      )}
      {loss.declared ? null : (
        <>
          {" "}
          — the citation named a section and no document, so which spec it meant is a guess, the
          same guess the rest of this panel makes about the present.
        </>
      )}
    </li>
  );
}
