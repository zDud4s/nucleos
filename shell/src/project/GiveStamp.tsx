// §spec mapa-do-projeto
import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useCarimbar, type Anchored } from "../data/project-map";

/**
 * The owner's verdict on one decision, as three buttons and a note (§5.2).
 *
 * **A module of its own because two piles now ask for it, and there are still only three
 * verdicts.** It began inside `StampsPanel.tsx`, whose header said it was the one surface in this mode
 * with buttons — true while the only thing that reached the owner was a stamp of theirs going
 * stale. Slice 5 added the other half of §5.3's `J`: the triager flags a decision nobody has
 * stamped, and §5.3 takes that decision out of `K` precisely because it has *arrived*. A flagged
 * row with no verdict on it would be a nag with no answer — and the answer is not a fourth verdict,
 * it is one of these three, so what had to be shared was the control and not the vocabulary.
 *
 * Copying it into the second panel was the alternative and is the worse one: the day the note's
 * rule changes, one of the two copies keeps the old one, and nothing on screen would look wrong.
 *
 * **The mutation is per row**, the way it always was: a refusal belongs to the row it was refused
 * about, and one shared mutation would put the last failure's sentence under whichever row happened
 * to be looking.
 *
 * **The note field is above the buttons and is always there.** The table refuses an empty amber and
 * the daemon answers `400`, so a middle button with nowhere to type is a button that can only fail
 * — a worse answer than a field. It is *disabled* until something is written rather than hidden:
 * hiding it would be this control deciding which of the three verdicts the owner is allowed to
 * give, and §6 reserves that to them. **Why it waits is said once by whoever draws the pile**, and
 * never here — three hundred copies of a true sentence is a wall nobody reads to the bottom of, and
 * that is how the one thing that mattered underneath goes unseen.
 *
 * The names carry the document and the section, because a screen full of buttons all called
 * "part-way" is a screen full of identical announcements to anybody not looking at it, and this is
 * a surface whose whole promise is that you know what you just answered.
 */

export interface GiveStampProps {
  projectId: string;
  row: Anchored;
}

export function GiveStamp({ projectId, row }: GiveStampProps) {
  const [note, setNote] = useState("");
  const carimbar = useCarimbar(projectId);

  const name = `${row.spec_slug} ${row.section}`;
  // Trimmed here against a daemon that trims before it checks, so the button is disabled for
  // exactly the notes the núcleo would refuse and for no others.
  const written = note.trim();
  const send = (verdict: "settled" | "partial" | "withdrawn") =>
    carimbar.mutate({
      decisionId: row.decision_id,
      verdict,
      // `null` and never `""`: *said nothing* and *said the empty string* are different, and the
      // núcleo stores the first as NULL.
      note: written === "" ? null : written,
    });

  return (
    <div className="flex flex-col gap-2">
      <input
        type="text"
        aria-label={`note for ${name}`}
        value={note}
        onChange={(event) => setNote(event.target.value)}
        placeholder="what is missing, in your words"
        className="rounded-md border border-border bg-surface-sunken px-2 py-1 text-xs text-text placeholder:text-text-faint"
      />
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          aria-label={`stamp ${name} as what you want`}
          disabled={carimbar.isPending}
          onClick={() => send("settled")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          as I want it
        </button>
        <button
          type="button"
          aria-label={`stamp ${name} as part-way`}
          disabled={carimbar.isPending || written === ""}
          onClick={() => send("partial")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          part-way, and I know
        </button>
        <button
          type="button"
          aria-label={`stamp ${name} as changed your mind`}
          disabled={carimbar.isPending}
          onClick={() => send("withdrawn")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
        >
          changed my mind
        </button>
      </div>
      {carimbar.isError ? <Refused error={carimbar.error} /> : null}
    </div>
  );
}

/**
 * Why a verdict did not land, with the row still on screen.
 *
 * The `503` is the sentence that had to be written carefully. It means git is there and would not
 * say what this decision is anchored to, and §7.1 makes *está como quero* the only verdict the code
 * moving can falsify — so it is the only one that may not be recorded without knowing what it is
 * watching. It is transient, a second attempt works, and the owner did nothing wrong. Copy that
 * read as a failure would put the blame for a busy git on the person who pressed the button, which
 * is the opposite of what this feature is buying.
 */
function Refused({ error }: { error: unknown }) {
  const box =
    "max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted";

  if (!isApiRefusal(error)) {
    return <p className={box}>The núcleo did not answer, so nothing was recorded.</p>;
  }

  if (error.status === 503) {
    return (
      <p className={box}>
        Git would not say what this decision is anchored to just now, so the green was not
        recorded. Nothing you asked for was wrong — try again in a moment.
      </p>
    );
  }
  if (error.status === 404) {
    return (
      <p className={box}>
        That decision is not yours to stamp now — it belongs to another project, or nobody approved
        it.
      </p>
    );
  }
  if (error.status === 400) {
    return <p className={box}>An amber needs a note, and this one arrived empty.</p>;
  }
  return <p className={box}>{error.detail}</p>;
}
