import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useExtractSpec, useProjectSpecs, type Brain } from "../data/project-map";

/**
 * Choosing what gets read, and by whom.
 *
 * The intention layer is not read out of the specs — it is **provoked** out of them (§4): the
 * convention of a decision table exists in two of thirty-eight documents, so a model reads one spec
 * and proposes the short list of decisions it fixes. What disarms the obvious objection — a model
 * reading a document written by a model — is the *shape of the product*: twelve numbered lines are
 * read in two minutes, and the step that does not happen today, the owner looking, starts happening.
 *
 * **The brain is the owner's choice, per request, and it is made here.** Not a setting: a setting
 * would turn the núcleo's refusal into somebody's permanent default. `local` is what somebody picks
 * when the document must not leave the machine or when they are not paying for it, and a machine
 * with no local model configured is refused rather than quietly sent to the cloud — so this surface
 * has a sentence for that refusal, because a generic failure would hide the single property that
 * makes the choice worth offering.
 *
 * Separate from {@link MapaPorAprovar} because these are two jobs: choosing what to read is not
 * answering what came back, and one file owning a picker, a brain, a mutation, a list and six
 * buttons would be the junk drawer this workspace is built against.
 */

export interface ExtrairSpecProps {
  projectId: string;
}

/**
 * Both, always, and `cloud` chosen first.
 *
 * The preselection is the one this machine is *certain* to be able to serve: the cloud brain goes
 * through the CLI that is already installed, and the local one exists only where a local model is
 * configured. Opening on `local` would make the ordinary first press a 503 on most machines, which
 * teaches somebody the feature is broken when what happened is that a promise was kept.
 *
 * It costs money, and that is why the choice is never silent: the pressed button says which brain
 * is chosen, the sentence under the pair says what that brain does with the document, and the
 * button that starts the read names the brain in its own label. Nothing about it is discovered
 * afterwards.
 */
const BRAINS: Brain[] = ["cloud", "local"];

const BRAIN_MEANING: Record<Brain, string> = {
  cloud: "The cloud brain reads it through the CLI installed on this machine. The document leaves the machine, and the read is billed.",
  // Deliberately not the words the 503 uses. The refusal's sentence has to be the ONLY place those
  // words can come from, or a test asserting on them would pass against a surface that never said
  // it — which is how the one property this feature exists to show would ship unguarded.
  local: "The local brain reads it through the model on this machine. Nothing leaves and nothing is billed — and where no such model is set up, the núcleo refuses rather than quietly reading it in the cloud instead.",
};

export function ExtrairSpec({ projectId }: ExtrairSpecProps) {
  const specs = useProjectSpecs(projectId);
  const extract = useExtractSpec(projectId);
  const [chosen, setChosen] = useState<string | null>(null);
  const [brain, setBrain] = useState<Brain>("cloud");

  if (specs.isError) {
    return (
      <p className="text-sm text-text-faint">
        The núcleo could not say which specs this project keeps — its folder may have moved.
      </p>
    );
  }
  if (specs.data === undefined) {
    return <p className="text-sm text-text-faint">Looking for this project&rsquo;s specs…</p>;
  }

  /*
    §11: a project with no specs is not broken, it is unconfigured in a way nobody has to configure
    — and the three folders are named because being told what is missing is the difference between
    a fixable state and a mystery. An empty picker with no sentence under it would be
    indistinguishable from one that failed to load, which is the false confidence again.
  */
  if (specs.data.length === 0) {
    return (
      <section aria-label="Read a spec" className="flex flex-col gap-2">
        <p className="max-w-prose text-sm text-text-muted">
          This project keeps no specs, so there is no intention layer to make — the structure above
          is the whole map it can have, and it is not wrong, only thin.
        </p>
        <p className="max-w-prose text-xs text-text-faint">
          The núcleo looks in <span className="font-mono">.ai/specs/</span>,{" "}
          <span className="font-mono">docs/specs/</span> and{" "}
          <span className="font-mono">docs/superpowers/specs/</span>. One document in any of them is
          all this needs.
        </p>
      </section>
    );
  }

  return (
    <section aria-label="Read a spec" className="flex flex-col gap-3">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        Read a spec
      </h2>
      <p className="max-w-prose text-sm text-text-muted">
        One model reads one document and proposes the decisions it fixes. Nothing it proposes enters
        the map: the list lands below and waits for you to answer it, line by line.
      </p>

      {/*
        By name, and never a box to type one into. Nobody is expected to remember
        `2026-08-24-mapa-do-projeto-design`, and a typed slug that matches nothing is a 404 that
        looks exactly like a document that is not there.
      */}
      <div
        role="group"
        aria-label="Specs"
        className="flex max-h-64 flex-col gap-1 overflow-y-auto rounded-md border border-border bg-surface-sunken p-2"
      >
        {specs.data.map((slug) => (
          <button
            key={slug}
            type="button"
            aria-pressed={slug === chosen}
            disabled={extract.isPending}
            onClick={() => setChosen(slug)}
            className={
              slug === chosen
                ? "truncate rounded-sm border border-accent bg-surface-raised px-2 py-1 text-left font-mono text-xs text-text"
                : "truncate rounded-sm px-2 py-1 text-left font-mono text-xs text-text-muted enabled:hover:text-text disabled:opacity-40"
            }
          >
            {slug}
          </button>
        ))}
      </div>

      <div role="group" aria-label="Which brain reads it" className="flex flex-wrap gap-2">
        {BRAINS.map((candidate) => (
          <button
            key={candidate}
            type="button"
            aria-pressed={candidate === brain}
            disabled={extract.isPending}
            onClick={() => setBrain(candidate)}
            className={
              candidate === brain
                ? "rounded-md border border-accent bg-surface-raised px-3 py-1.5 text-sm text-text"
                : "rounded-md border border-border px-3 py-1.5 text-sm text-text-muted enabled:hover:border-border-strong disabled:opacity-40"
            }
          >
            {candidate}
          </button>
        ))}
      </div>
      <p className="max-w-prose text-xs text-text-faint">{BRAIN_MEANING[brain]}</p>

      <div className="flex flex-wrap items-center gap-3">
        {/*
          The label names both halves of what is about to happen. A button reading "extract" would
          leave the brain — the expensive half, and the half the owner asked to control — visible
          only in a pressed state three lines above it.
        */}
        <button
          type="button"
          disabled={chosen === null || extract.isPending}
          onClick={() => {
            if (chosen === null) return;
            extract.mutate({ specSlug: chosen, brain });
          }}
          className="rounded-md border border-border px-3 py-1.5 text-sm text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          read {chosen ?? "the chosen spec"} with the {brain} brain
        </button>
      </div>

      {/*
        The wait, said out loud. `POST /map/extract` is synchronous on purpose — the owner pressed a
        button about one document and the list is the answer — and a surface that went quiet for a
        minute would look broken at exactly the moment it is working.
      */}
      {extract.isPending ? (
        <p className="max-w-prose text-xs text-text-muted">
          Reading {chosen} with the {brain} brain. This is a whole document going through a model in
          one request, and the answer arrives only when it has finished — a cloud read can take a
          minute.
        </p>
      ) : null}

      {extract.isError ? <Failed error={extract.error} /> : null}

      {/*
        What came back is the WHOLE pile and not this extraction — the route says so, because a
        window handed only the new rows would show a shrinking list every time a second spec was
        read. The sentence counts what is waiting, which is the number that is true.
      */}
      {extract.isSuccess && extract.data !== undefined ? (
        <p className="max-w-prose text-xs text-text-muted">
          {extract.data.length === 0
            ? "Read — and nothing is waiting below. The model pointed at no decision it could name a section for, which is a real answer and not a failure."
            : `Read. ${extract.data.length} ${extract.data.length === 1 ? "line is" : "lines are"} waiting below, this document's among them, and not one of them is in the map until you answer it.`}
        </p>
      ) : null}
    </section>
  );
}

/**
 * Why the read did not happen, as what it actually was.
 *
 * **503 gets its own sentence and always will.** It is the núcleo refusing to substitute one brain
 * for another, which is the single most important property of this feature; folded into a generic
 * failure it would read as a broken daemon, and the owner would learn to distrust the one refusal
 * that is protecting both their bill and their document.
 *
 * The route refuses with a bare status and no name of its own, so these switch on `status` rather
 * than on `code` — the derived code for a 503 is `unavailable`, which is the floor under a name and
 * not a name. The same reading `ModeCodigo`'s own refusal note takes.
 */
function Failed({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <p className="max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
        The núcleo did not answer, so nothing was read and nothing was recorded.
      </p>
    );
  }

  const sentence =
    error.status === 503
      ? "This machine has no local model configured, so the local brain has nothing to read with. The núcleo refuses rather than sending the document to a cloud you did not ask for — which is the whole reason the choice is yours to make."
      : error.status === 502
        ? "A model was reached and the read failed. Nothing was recorded: an empty list would have claimed this document decides nothing, and nobody is in a position to claim that when nobody read it."
        : error.status === 404
          ? "That document is not there any more, or it cannot be read."
          : error.status === 422
            ? "The núcleo could not read which brain was asked for."
            : error.detail;

  return (
    <p className="max-w-prose rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
      {sentence}
    </p>
  );
}
