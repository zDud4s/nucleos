// §spec mapa-do-projeto
import { useId, useState } from "react";
import { isApiRefusal } from "../data/client";
import { useExtractSpec, useProjectSpecs, type Brain } from "../data/project-map";
import { Button, ErrorNote, Segmented, Select } from "../ui";
import { DAEMON_NAME } from "../copy";

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
 * Separate from {@link DecisionsWaiting} because these are two jobs: choosing what to read is not
 * answering what came back, and one file owning a picker, a brain, a mutation, a list and six
 * buttons would be the junk drawer this workspace is built against.
 */

export interface ExtractSpecProps {
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
 * is chosen, the line under the row says the cloud read is billed whenever cloud is chosen, and the
 * button that starts the read names the brain in its own label. Nothing about it is discovered
 * afterwards.
 */
const BRAINS: Brain[] = ["cloud", "local"];

const CLOUD_COST = "Cloud: the document leaves this machine, and the read is billed.";

export function ExtractSpec({ projectId }: ExtractSpecProps) {
  const specs = useProjectSpecs(projectId);
  const extract = useExtractSpec(projectId);
  const [chosen, setChosen] = useState<string | null>(null);
  const [brain, setBrain] = useState<Brain>("cloud");
  const pickId = useId();

  if (specs.isError) {
    return (
      <ErrorNote>
        The {DAEMON_NAME} could not say which specs this project keeps: its folder may have moved.
      </ErrorNote>
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
        <p
          className="text-sm text-text-muted"
          title={`No specs, so no intention layer. The ${DAEMON_NAME} looks in docs/specs/ and docs/superpowers/specs/; one document in either is enough.`}
        >
          No specs
        </p>
      </section>
    );
  }

  return (
    <section aria-label="Read a spec" className="flex flex-col gap-2">
      {/*
        One row: what is read, by whom, and the act. It was a scrolling list of every spec, a brain
        switch under it and the button alone at the far edge — three rows for one decision.
        Still by name, and never a box to type one into: nobody is expected to remember
        `2026-08-24-mapa-do-projeto-design`, and a typed slug that matches nothing is a 404 that
        looks exactly like a document that is not there.
      */}
      <div className="ws-read">
        <label className="ws-read-label" htmlFor={pickId}>
          Read a spec
        </label>
        <Select
          id={pickId}
          density="sm"
          mono
          value={chosen ?? ""}
          disabled={extract.isPending}
          onChange={(event) => setChosen(event.target.value === "" ? null : event.target.value)}
        >
          <option value="">Choose one of {specs.data.length}…</option>
          {specs.data.map((slug) => (
            <option key={slug} value={slug}>
              {slug}
            </option>
          ))}
        </Select>
        <Segmented
          label="Which brain reads it"
          value={brain}
          options={BRAINS.map((candidate) => ({
            value: candidate,
            label: candidate,
            disabled: extract.isPending,
          }))}
          onChange={setBrain}
        />
        {/*
          The label names the brain, the expensive half and the half the owner asked to control —
          a button reading "extract" would leave it visible only in a pressed state beside it. Its
          accessible name carries the document too, which the select beside it shows only by eye.
        */}
        <Button
          variant="ghost"
          aria-label={`read ${chosen ?? "the chosen spec"} with the ${brain} brain`}
          title={brain === "cloud" ? CLOUD_COST : undefined}
          disabled={chosen === null || extract.isPending}
          onClick={() => {
            if (chosen === null) return;
            extract.mutate({ specSlug: chosen, brain });
          }}
        >
          Read with the {brain} brain
        </Button>
      </div>

      {/*
        The wait, said out loud. `POST /map/extract` is synchronous on purpose — the owner pressed a
        button about one document and the list is the answer — and a surface that went quiet for a
        minute would look broken at exactly the moment it is working.
      */}
      {extract.isPending ? (
        <p
          className="text-xs text-text-muted"
          title="A whole document goes through a model in one request. A cloud read can take a minute."
        >
          Reading {chosen} with the {brain} brain…
        </p>
      ) : null}

      {extract.isError ? <Failed error={extract.error} /> : null}

      {/*
        What came back is the WHOLE pile and not this extraction — the route says so, because a
        window handed only the new rows would show a shrinking list every time a second spec was
        read. The sentence counts what is waiting, which is the number that is true.
      */}
      {extract.isSuccess && extract.data !== undefined ? (
        <p
          className="text-xs text-text-muted"
          title="None of them is in the map until you answer it."
        >
          {extract.data.length === 0
            ? "Read: nothing waiting."
            : `Read: ${extract.data.length} waiting below.`}
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
 * not a name. The same reading `ModeCode`'s own refusal note takes.
 */
function Failed({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        The {DAEMON_NAME} gave no reply, so nothing was read and nothing was recorded.
      </ErrorNote>
    );
  }

  const sentence =
    error.status === 503
      ? "This machine has no local model configured. Nothing was sent to the cloud."
      : error.status === 502
        ? "A model was reached and the read failed. Nothing was recorded."
        : error.status === 404
          ? "That document is not there any more, or it cannot be read."
          : error.status === 422
            ? `The ${DAEMON_NAME} could not read which brain was asked for.`
            : error.detail;

  return <ErrorNote>{sentence}</ErrorNote>;
}
