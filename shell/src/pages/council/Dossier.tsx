import { useState, type ReactNode } from "react";
import { canTakeASeat, useAgents } from "../../data/agents";
import { useAssistantModels } from "../../data/chats";
import {
  deanonymise,
  seatName,
  type CouncilConfig,
  type CouncilView,
  type StepView,
} from "../../data/council";
import { Button, ErrorNote, Skeleton } from "../../ui";
import { CouncilRich } from "../CouncilRich";
import { CouncilSynthesis } from "../CouncilSynthesis";
import { StepTail } from "../CouncilRounds";
import type { CourtSelection } from "./Courtroom";
import {
  release,
  seatAt,
  seatOfModel,
  withJudge,
  withRole,
  type CourtDraft,
  type Occupant,
} from "./court-draft";
import { type Court, type CourtSeat, type Moment } from "./court-model";

/**
 * What is written on the file the room points at: the case, a chair, the
 * bench. In setup it is where chairs are filled and roles handed out; in a
 * session it is where the text of a seat, or the verdict, is read.
 */
export type DossierProps =
  | {
      mode: "setup";
      selection: CourtSelection;
      draft: CourtDraft;
      question: string;
      court: Court;
      config: CouncilConfig | null;
      /** Why the config blocks convening (`configured: false` and the like). */
      refusal: ReactNode;
      onDraft: (draft: CourtDraft) => void;
      onClear: () => void;
    }
  | {
      mode: "session";
      view: CouncilView;
      court: Court;
      moment: Moment;
      selection: CourtSelection;
      following: boolean;
      onFollowLive: () => void;
    };

export function Dossier(props: DossierProps) {
  return props.mode === "setup" ? <SetupDossier {...props} /> : <SessionDossier {...props} />;
}

/**
 * Who chaired, and on what.
 *
 * A deleted chairman falls back to its id rather than to nothing: an id is
 * ugly and is still an answer to "who". `chairman_ref` is printed whether or
 * not an agent chaired, because the model is the fact that survives — an
 * agent can be renamed or deleted, and the row keeps the model that answered.
 */
export function chairmanLine(view: CouncilView): string {
  const named = view.chairman_agent_name ?? view.chairman_agent_id;
  if (named === null) return `chaired by ${view.chairman_ref}`;
  return `chaired by ${named} on ${view.chairman_ref}`;
}

/* ----------------------------------------------------------------- shared -- */

/** The room's own readings, for when the room hides them (narrow screens hide `.council-court-small`). */
function Summary({ court }: { court: Court }) {
  const { balance, wing, ballots } = court;
  return (
    <dl className="council-dossier-summary">
      <dt>Scales</dt>
      <dd>{balance.sub === null ? balance.label : `${balance.label} · ${balance.sub}`}</dd>
      <dt>Room</dt>
      <dd>{wing.sub === null ? wing.title : `${wing.title} · ${wing.sub}`}</dd>
      {ballots !== null && (
        <>
          <dt>Ballots</dt>
          <dd>{`ballots ${ballots.got}/${ballots.total}`}</dd>
        </>
      )}
    </dl>
  );
}

const roleWords = (role: string) => role.replace(/_/g, " ");

/* ------------------------------------------------------------------ setup -- */

function SetupDossier({
  selection,
  draft,
  question,
  court,
  config,
  refusal,
  onDraft,
  onClear,
}: Extract<DossierProps, { mode: "setup" }>) {
  let body: ReactNode;
  if (selection?.kind === "seat") {
    const chair = draft.chairs[selection.index] ?? null;
    const at = selection.index;
    body =
      chair === null ? (
        <section className="council-dossier-section" aria-labelledby="council-dossier-title">
          <h2 id="council-dossier-title" className="council-dossier-title">{`Call to seat ${at + 1}`}</h2>
          <ConveneChips onPick={(who) => onDraft(seatAt(draft, at, who))} />
          <Roles config={config} current={draft.roles[at] ?? null} onRole={(role) => onDraft(withRole(draft, at, role))} />
        </section>
      ) : (
        <OccupiedSeat
          key={at}
          at={at}
          chair={chair}
          draft={draft}
          config={config}
          onDraft={onDraft}
          onClear={onClear}
        />
      );
  } else if (selection?.kind === "judge") {
    body = (
      <section className="council-dossier-section" aria-labelledby="council-dossier-title">
        <h2 id="council-dossier-title" className="council-dossier-title">Chairman</h2>
        <p className="council-dossier-who">{draft.chairman === null ? "Nobody presides yet." : draft.chairman.name}</p>
        <ConveneChips onPick={(who) => onDraft(withJudge(draft, who))} />
      </section>
    );
  } else {
    body = (
      <section className="council-dossier-section" aria-labelledby="council-dossier-title">
        <h2 id="council-dossier-title" className="council-dossier-title">The case</h2>
        {question.trim() === "" ? (
          <p className="council-dossier-hint">Write the question on the lectern.</p>
        ) : (
          <p className="council-dossier-question">{question}</p>
        )}
        {refusal}
      </section>
    );
  }
  return (
    <aside className="council-dossier" aria-label="Dossier">
      {body}
      <Summary court={court} />
    </aside>
  );
}

function OccupiedSeat({
  at,
  chair,
  draft,
  config,
  onDraft,
  onClear,
}: {
  at: number;
  chair: Occupant;
  draft: CourtDraft;
  config: CouncilConfig | null;
  onDraft: (draft: CourtDraft) => void;
  onClear: () => void;
}) {
  const [swapping, setSwapping] = useState(false);
  return (
    <section className="council-dossier-section" aria-labelledby="council-dossier-title">
      <h2 id="council-dossier-title" className="council-dossier-title">{`Seat ${at + 1}`}</h2>
      <p className="council-dossier-who">{chair.name}</p>
      <div className="council-dossier-actions">
        <Button aria-expanded={swapping} onClick={() => setSwapping(!swapping)}>
          Swap
        </Button>
        <Button
          variant="quiet"
          onClick={() => {
            onDraft(release(draft, at));
            onClear();
          }}
        >
          Release
        </Button>
      </div>
      {swapping && (
        <ConveneChips
          onPick={(who) => {
            onDraft(seatAt(draft, at, who));
            setSwapping(false);
          }}
        />
      )}
      <Roles config={config} current={draft.roles[at] ?? null} onRole={(role) => onDraft(withRole(draft, at, role))} />
    </section>
  );
}

/**
 * The badges a chair may wear: the closed set of roles the daemon declares,
 * plus none. Roles are not exclusive and do not by themselves touch the roster.
 */
function Roles({
  config,
  current,
  onRole,
}: {
  config: CouncilConfig | null;
  current: string | null;
  onRole: (role: string | null) => void;
}) {
  const roles = config?.roles ?? [];
  if (roles.length === 0) return null;
  return (
    <div className="council-dossier-chips" role="group" aria-label="Roles">
      {roles.map((role) => (
        <button
          key={role}
          type="button"
          className="council-dossier-chip council-dossier-chip-role"
          aria-pressed={current === role}
          onClick={() => onRole(role)}
        >
          {roleWords(role)}
        </button>
      ))}
      <button
        type="button"
        className="council-dossier-chip"
        aria-pressed={current === null || current === ""}
        onClick={() => onRole(null)}
      >
        no role
      </button>
    </div>
  );
}

/**
 * Who may be called to a chair: agents and models, as chips. This is the one
 * place the two catalogues are read, so nothing asks for either until a chair
 * or the bench is chosen.
 */
function ConveneChips({ onPick }: { onPick: (who: Occupant) => void }) {
  const agents = useAgents();
  const models = useAssistantModels();

  // `canTakeASeat` and not a rule written here: it exists to answer this exact
  // question and is the shell's reading of `council.rs:849` — a seat's row
  // records the model that answered, `NOT NULL`, so an agent naming no model
  // is a refusal waiting to happen. Offering it would be offering a 400.
  const seatable = (agents.data ?? []).filter(canTakeASeat);
  const choices = models.data?.choices ?? [];
  const waiting = agents.data === undefined && models.data === undefined && !agents.isError && !models.isError;
  // One catalogue down still leaves the other's chips; both down would leave an empty panel that reads as "nobody".
  const unreadable = agents.isError && models.isError;
  // Exactly one down: the other's chips stay, and the missing half is named so it does not read as "none exist".
  const oneDown = agents.isError !== models.isError;

  return (
    <>
      {waiting && <Skeleton label="Reading who can sit" lines={2} />}
      {unreadable && (
        <ErrorNote
          onRetry={() => {
            void agents.refetch();
            void models.refetch();
          }}
        >
          Couldn't load who can sit.
        </ErrorNote>
      )}
      {oneDown && (
        <ErrorNote
          onRetry={() => {
            void (agents.isError ? agents.refetch() : models.refetch());
          }}
        >
          {agents.isError ? "Couldn't load agents." : "Couldn't load models."}
        </ErrorNote>
      )}
      {seatable.length > 0 && (
        <div className="council-dossier-chips" role="group" aria-label="Agents">
          {seatable.map((agent) => (
            <button
              key={agent.id}
              type="button"
              className="council-dossier-chip"
              onClick={() => onPick({ seat: { agent: agent.id }, name: agent.name })}
            >
              {agent.name}
            </button>
          ))}
        </div>
      )}
      {choices.length > 0 && (
        <div className="council-dossier-chips" role="group" aria-label="Models">
          {choices.map((model) => (
            <button
              key={model.id}
              type="button"
              className="council-dossier-chip"
              onClick={() => onPick({ seat: seatOfModel(model), name: model.label })}
            >
              {model.label}
            </button>
          ))}
        </div>
      )}
    </>
  );
}

/* ---------------------------------------------------------------- session -- */

function SessionDossier({
  view,
  court,
  moment,
  selection,
  following,
  onFollowLive,
}: Extract<DossierProps, { mode: "session" }>) {
  const chosen = selection?.kind === "seat" ? court.seats.find((s) => s.index === selection.index) : undefined;
  let body: ReactNode;
  // The Verdict section renders the error itself; anywhere else an `error` council still owes it.
  let verdictShown = false;
  if (chosen !== undefined) {
    body = <SeatNotes view={view} seat={chosen} />;
  } else if (selection?.kind === "lectern") {
    body = (
      <section className="council-dossier-section" aria-labelledby="council-dossier-title">
        <h2 id="council-dossier-title" className="council-dossier-title">The case</h2>
        <p className="council-dossier-question">{view.question}</p>
      </section>
    );
  } else if (selection?.kind === "judge" || moment.phase === "synthesis") {
    verdictShown = true;
    body = <Verdict view={view} />;
  } else {
    body = (
      <section className="council-dossier-section" aria-labelledby="council-dossier-title">
        <h2 id="council-dossier-title" className="council-dossier-title">The case</h2>
        <p className="council-dossier-question">{view.question}</p>
        <p className="council-dossier-hint">Choose a seat to read what it said.</p>
      </section>
    );
  }
  return (
    <aside className="council-dossier" aria-label="Dossier">
      {!following && (
        <div className="council-dossier-actions">
          <Button variant="link" onClick={onFollowLive}>
            Follow live
          </Button>
        </div>
      )}
      {view.status === "error" && !verdictShown && <ErrorNote>{view.error ?? "the council failed"}</ErrorNote>}
      {body}
      <Summary court={court} />
    </aside>
  );
}

function Verdict({ view }: { view: CouncilView }) {
  const name = view.chairman_agent_name ?? view.chairman_agent_id ?? view.chairman_ref;
  const writing = view.status === "running" && view.current_phase === "synthesis";
  let body: ReactNode;
  if (view.status === "done") {
    body =
      view.synthesis === null && view.synthesis_structured === null && view.error === null ? (
        <p className="council-dossier-hint">transcript expired</p>
      ) : (
        <CouncilSynthesis view={view} />
      );
  } else if (view.status === "error") {
    body = <ErrorNote>{view.error ?? "the council failed"}</ErrorNote>;
  } else if (view.status === "cancelled") {
    body = <p className="council-dossier-hint">council cancelled</p>;
  } else if (writing) {
    body =
      view.synthesis_run_id !== null ? (
        <StepTail key={view.synthesis_run_id} runId={view.synthesis_run_id} name={name} />
      ) : (
        <p className="council-dossier-hint">The chair is writing the verdict…</p>
      );
  } else {
    body = <p className="council-dossier-hint">The chair has not spoken yet.</p>;
  }
  return (
    <section className="council-dossier-section" aria-labelledby="council-dossier-title">
      <h2 id="council-dossier-title" className="council-dossier-title">Verdict</h2>
      <p className="council-dossier-who">{chairmanLine(view)}</p>
      {body}
    </section>
  );
}

const STATE_LINE: Record<string, string> = {
  waiting: "has not spoken yet",
  thinking: "thinking…",
  speaking: "speaking",
  ok: "done",
  cancelled: "cancelled",
  skipped: "skipped",
};

function SeatNotes({ view, seat }: { view: CouncilView; seat: CourtSeat }) {
  const record = view.seats.find((s) => s.seat_idx === seat.index);
  const name = record === undefined ? seat.name : seatName(record);
  const title = seat.masked ? seat.label : seat.name;
  // A masked seat is its letter everywhere, the accessible names included.
  const voter = seat.masked ? seat.label : name;
  const state = seat.state === "failed" ? `failed: ${seat.reason ?? "failed"}` : (STATE_LINE[seat.state] ?? seat.state);
  return (
    <section className="council-dossier-section" aria-labelledby="council-dossier-title">
      <h2 id="council-dossier-title" className="council-dossier-title" title={seat.masked ? `${seat.name} (masked in this round)` : undefined}>
        {title}
      </h2>
      <p className="council-dossier-who">{[`Seat ${seat.index + 1}`, seat.sublabel, state].filter(Boolean).join(" · ")}</p>
      {seat.state === "speaking" && seat.runId !== null ? (
        <StepTail key={seat.runId} runId={seat.runId} name={voter} />
      ) : seat.state === "failed" ? (
        <ErrorNote>{seat.reason ?? "failed"}</ErrorNote>
      ) : (
        seat.step !== undefined && <StepText view={view} step={seat.step} voter={voter} />
      )}
    </section>
  );
}

/** What a settled step said: an answer, a ballot, or a revision. */
function StepText({ view, step, voter }: { view: CouncilView; step: StepView; voter: string }) {
  if (step.status !== "ok") return null;
  const nameOf = (seatIdx: number | null): string => {
    const found = seatIdx === null ? undefined : view.seats.find((s) => s.seat_idx === seatIdx);
    return found === undefined ? "an unrecorded seat" : seatName(found);
  };
  if (step.phase === "critique") {
    const critique = step.critique;
    if (critique === null) return <p className="council-dossier-hint">no readable critique for this round.</p>;
    if (critique.ranking.length === 0) return <p className="council-dossier-hint">abstained</p>;
    return (
      <>
        <ol className="council-dossier-ballot" aria-label={`Ballot of ${voter}`}>
          {deanonymise(critique.ranking, view.anon_map).map((idx, at) => (
            <li key={at}>{nameOf(idx)}</li>
          ))}
        </ol>
        {critique.reviews.map((review, at) => {
          const [target] = deanonymise([review.label], view.anon_map);
          return (
            <div className="council-dossier-review" key={`${review.label}-${at}`}>
              <p className="council-dossier-review-on">{`On ${nameOf(target)}'s answer`}</p>
              <ul>
                {review.points.map((point, i) => (
                  <li key={i}>{`${point.stance}: ${point.claim} — ${point.why}`}</li>
                ))}
              </ul>
            </div>
          );
        })}
      </>
    );
  }
  if (step.phase === "revise") {
    const verdict =
      step.changed === true ? "changed its answer" : step.changed === false ? "kept its answer" : "revision not readable";
    return (
      <>
        <p className="council-dossier-hint">{step.why !== null && step.why !== "" ? `${verdict}: ${step.why}` : verdict}</p>
        {step.changed === true &&
          (step.answer !== null ? <CouncilRich text={step.answer} /> : <p className="council-dossier-hint">transcript expired</p>)}
      </>
    );
  }
  if (step.answer === null) return <p className="council-dossier-hint">transcript expired</p>;
  return <CouncilRich text={step.answer} />;
}
