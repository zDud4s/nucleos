import { useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { canTakeASeat, useAgents } from "../data/agents";
import { useAssistantModels } from "../data/chats";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCancelCouncil,
  useCouncil,
  useCouncilOutcome,
  outcomeOf,
  useCouncils,
  useCreateCouncil,
  useCouncilConfig,
  readCouncilConfig,
  councilIsAlive,
  type CouncilSummary,
  type CouncilView,
} from "../data/council";
import { CouncilDeliberation } from "./CouncilRounds";
import { clearDraft, draftFrom, offerDraft, peekDraft } from "./council-draft";
import { Courtroom, type CourtSelection } from "./council/Courtroom";
import { Dossier, chairmanLine } from "./council/Dossier";
import { ProcessLine } from "./council/ProcessLine";
import {
  conveneBlocked,
  draftFromAskAgain,
  draftFromConfig,
  pruneUnoffered,
  requestFrom,
  roundsOf,
  type CourtDraft,
} from "./council/court-draft";
import { chairsOf, rememberChairs } from "./council/court-chairs";
import { buildSessionCourt, buildSetupCourt, liveMoment, momentKey, processLine } from "./council/court-model";
import {
  Button,
  ConfirmButton,
  Count,
  Crumb,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./council.css";

/**
 * Council — one component serving `/council` and `/council/$councilId`. The
 * list and the courtroom where a council is assembled are the first; an open
 * council replaces both, with a crumb back to the list above its header, so the
 * detail is read on its own.
 *
 * The detail is the same room, now in session: the chairs show who is thinking,
 * who is speaking and who has spoken, the dossier beside it opens whatever is
 * pointed at (and follows whoever speaks until a click pins it), and the line
 * of phases under it goes back to how the room stood at each one. The
 * deliberation's columns stay available, shut, under "Case record".
 *
 * Progress is told as a round and a phase — "round 1 · critique" — because a
 * council runs as many critique rounds as it was asked for, and a fixed
 * "phase N of M" counter has no meaning over rounds. How many rounds there are
 * is the council's own fact (`rounds`), never this page's assumption.
 */
export function Council() {
  const params = useParams({ strict: false }) as { councilId?: string };
  const councilId = params.councilId ?? null;

  const councils = useCouncils();
  const rows = councils.data ?? [];
  const stale = councils.isError && councils.data !== undefined;

  return (
    <>
      {councilId !== null && <Crumb to="/council">Councils</Crumb>}
      <PageHeader title="Council" headline={headlineFor(rows, councils.data !== undefined)} />

      {councilId === null ? (
        <>
          <CourtSetup />

          {stale && <StaleNote dataUpdatedAt={councils.dataUpdatedAt} />}
          {councils.isError && councils.data === undefined && <ListError error={councils.error} />}

          <CouncilList rows={rows} answered={councils.data !== undefined} />

          {councils.data !== undefined && rows.length === 0 && (
            <Teach title="No council has met yet">
              <p>Ask a question above. Each seat answers on its own, and the chair writes one answer.</p>
            </Teach>
          )}
        </>
      ) : (
        <CouncilDetail key={councilId} id={councilId} />
      )}
    </>
  );
}

/** One derived sentence about the whole list. */
function headlineFor(rows: CouncilSummary[], answered: boolean): string | undefined {
  if (!answered) return undefined;
  if (rows.length === 0) return "no council has been convened";
  const noun = rows.length === 1 ? "council" : "councils";
  const running = rows.filter((row) => row.status === "running").length;
  return running === 0
    ? `${rows.length} ${noun}, none deliberating`
    : `${rows.length} ${noun}, ${running} deliberating`;
}

function ListError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the councils</ErrorNote>;
}

/**
 * The daemon's own sentence, when it really sent one.
 *
 * `client.ts` falls back to `statusText` for a refusal with an empty body, so a
 * bare status arrives carrying only the status word — four words is the floor
 * between that and a sentence the daemon wrote on purpose, such as the budget
 * refusal naming the limit and the spend.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------- convene -- */

/** The line in the case file when nothing is in the way of convening. */
const COMPOSER_HELPER =
  "Each seat answers on its own, ranks the others blind, and the chair writes one answer. Roster from ~/.nucleos/council.yaml.";

/**
 * The courtroom while a council is being assembled: the lectern for the
 * question, eight chairs and the bench for who sits, the dossier for choosing
 * them, and the line of phases with the rounds and Convene under it.
 */
function CourtSetup() {
  /*
   * "Ask again" leaves a draft behind. The initialisers only peek at it and the
   * mount effect drops it, so StrictMode's second initialiser run still sees it
   * and a later visit to the page starts empty.
   */
  const [question, setQuestion] = useState(() => peekDraft()?.question ?? "");
  /**
   * The rounds somebody picked, or `null` while they have picked none.
   *
   * `null` and not the config's default copied into state: the config answers
   * after the page mounts, so a copy taken at mount would be the wrong number,
   * and "untouched" has to stay distinguishable from "chose the default" for
   * the request to keep leaving the key off.
   */
  const [pickedRounds, setPickedRounds] = useState<number | null>(() => peekDraft()?.rounds ?? null);
  // `null` = untouched: the room is derived from the config on every render, so a
  // config that answers after mount still seats its roster. The first edit
  // materialises it. An untouched room asks the daemon for `{ question }` and
  // nothing else — the request this page made for its entire life before the
  // room existed — and asks for neither catalogue: they are read only once a
  // chair or the bench is chosen.
  const [edited, setEdited] = useState<CourtDraft | null>(() => {
    const offered = peekDraft();
    return offered === null ? null : draftFromAskAgain(offered);
  });
  // Only a room filled from an "Ask again" draft can hold a seat the catalogues
  // have since dropped. A configured seat is the file's to name, and pruning it
  // against the assistant's menu would unseat it on the first edit.
  const [fromAskAgain] = useState(() => peekDraft() !== null);
  const [selection, setSelection] = useState<CourtSelection>(null);
  useEffect(() => clearDraft(), []);

  const create = useCreateCouncil();
  const navigate = useNavigate();
  // One cheap read on every visit, unlike the catalogues: the bounds it carries
  // are what a council convened from here will run under, and saying so before
  // the question is asked is the point of the endpoint. Read through the shape
  // guard, because `apiFetch` casts whatever a `200` carried.
  const configQuery = useCouncilConfig();
  const config = configQuery.data === undefined ? null : readCouncilConfig(configQuery.data);
  const draft = edited ?? draftFromConfig(config);
  const rounds = roundsOf(pickedRounds, config);
  const configPending = configQuery.data === undefined && configQuery.isPending;
  // Failed only when nothing usable is cached: a background refetch that
  // errors leaves the last good config in `data`, and it still holds.
  const configFailed = configQuery.isError && config === null;
  const reason = conveneBlocked(configPending, configFailed, config, draft);
  const disabled = question.trim() === "" || create.isPending || reason !== null;

  // The button and ctrl+enter share this path, so the keyboard can never send
  // what the disabled button would have refused.
  const submit = () => {
    if (disabled) return;
    create.mutate(requestFrom(question, draft, pickedRounds, config), {
      onSuccess: (result) => {
        setQuestion("");
        rememberChairs(result.id, draft);
        void navigate({ to: `/council/${result.id}` });
      },
    });
  };

  const court = buildSetupCourt(config, draft, question, rounds);
  return (
    <>
      <section className="council-court-page" aria-label="Court">
        <Courtroom
          court={court}
          mode="setup"
          selected={selection}
          onSelect={setSelection}
          question={{ value: question, onChange: setQuestion, onSubmit: submit }}
        />
        <Dossier
          mode="setup"
          selection={selection}
          draft={draft}
          question={question}
          court={court}
          config={config}
          refusal={<p className="council-dossier-hint">{reason ?? COMPOSER_HELPER}</p>}
          onDraft={setEdited}
          onClear={() => setSelection(null)}
        />
        <ProcessLine
          mode="setup"
          rounds={rounds}
          maxRounds={config?.max_rounds}
          onRounds={setPickedRounds}
          convene={{ disabled, reason, onConvene: submit, pending: create.isPending }}
        />
      </section>
      {fromAskAgain && edited !== null && <OfferedSeats draft={edited} onDraft={setEdited} />}
      {create.isError && <ConveneRefusal error={create.error} />}
    </>
  );
}

/**
 * Once both catalogues have answered, a seat they no longer offer — one an "Ask
 * again" draft brought back — becomes an empty chair. Renders nothing and mounts
 * only for a room an "Ask again" filled, which is what keeps `/agents` and
 * `/assistant/models` off the page for everybody using the configured roster.
 * Only a real change is written back, so the effect settles after one pass.
 */
function OfferedSeats({ draft, onDraft }: { draft: CourtDraft; onDraft: (draft: CourtDraft) => void }) {
  const agents = useAgents();
  const models = useAssistantModels();
  const answered = agents.data !== undefined && models.data !== undefined;
  useEffect(() => {
    if (!answered) return;
    // `canTakeASeat` and not a rule written here: an agent naming no model is
    // a refusal waiting to happen (`council.rs:849`), so it is never offered.
    const agentIds = new Set((agents.data ?? []).filter(canTakeASeat).map((agent) => agent.id));
    const modelIds = new Set((models.data?.choices ?? []).map((model) => model.id));
    const next = pruneUnoffered(draft, agentIds, modelIds);
    if (next !== draft) onDraft(next);
  });
  return null;
}

/**
 * Why convening was refused.
 *
 * The daemon's own sentence is let through for every code here, because
 * `post_council` refuses with prose rather than a name — a 503 says there is no
 * roster configured, a 429 names the budget limit and how much of it is spent,
 * and a 400 says what was wrong with the question. Generic copy for any of
 * those would throw away the one part of the refusal worth reading.
 */
function ConveneRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — no council was convened</ErrorNote>;
  return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
}

/* ------------------------------------------------------------------- list -- */

function CouncilList({ rows, answered }: { rows: CouncilSummary[]; answered: boolean }) {
  if (answered && rows.length === 0) return null;

  return (
    <Panel title="Councils" aside={<Count n={answered ? rows.length : undefined} />}>
      {/* A wait and an absence, and they must not read the same. The loading
          line is faint prose about to be replaced by the list; `Quiet` is the
          answer that there is no list, which is the panel's content and is set
          at the rung content is set at. */}
      {!answered && <p className="council-loading">reading the councils…</p>}
      {rows.length > 0 && (
        <Rows label="Councils">
          {rows.map((row, index) => (
            <CouncilRow key={row.id} row={row} readOutcome={index < OUTCOME_ROWS} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

/**
 * Where a council is: "round 1 · critique". Round 0 is the seats answering on
 * their own, and the synthesis is named by its phase alone because it belongs
 * to no round.
 */
function progressOf(council: { current_round: number; current_phase: string }): string {
  if (council.current_phase === "synthesis") return "synthesis";
  return `round ${council.current_round} · ${council.current_phase}`;
}

/**
 * How many of the newest rows read their own outcome. The summary carries no
 * error, synthesis or agreement, so each outcome is one detail read; past the
 * tenth a row shows its badge alone rather than the list costing fifty reads.
 */
const OUTCOME_ROWS = 10;

/**
 * One council in the list, as a whole-row link. The list is only drawn while no
 * council is open, so no row is ever the current one.
 */
function CouncilRow({ row, readOutcome }: { row: CouncilSummary; readOutcome: boolean }) {
  const running = councilIsAlive(row.status);
  // A running council's detail is still moving and is read on its own page,
  // polled; the row says where it is from the summary alone.
  const outcome = useCouncilOutcome(row.id, readOutcome && !running);
  const ended = outcome.data !== undefined ? outcomeOf(outcome.data) : null;
  const second = running ? progressOf(row) : ended;
  return (
    <Row>
      <Link className="council-row-link" to={`/council/${row.id}`}>
        <span className="council-row-question">{row.question}</span>
        <StateBadge domain="council" state={row.status} />
        <RelativeTime at={row.created_at} />
        {second !== null && <span className="council-row-outcome">{second}</span>}
      </Link>
    </Row>
  );
}

/* ----------------------------------------------------------------- detail -- */

function CouncilDetail({ id }: { id: string }) {
  const council = useCouncil(id);
  const detail = council.data;

  if (detail === undefined) {
    return (
      <Panel title="Council">
        {council.isError ? (
          <DetailError error={council.error} />
        ) : (
          <p className="council-loading">reading the council…</p>
        )}
      </Panel>
    );
  }

  return <CouncilSession id={id} detail={detail} />;
}

/** A council that exists: the room in session, the line of phases, and the record shut beneath. */
function CouncilSession({ id, detail }: { id: string; detail: CouncilView }) {
  const cancel = useCancelCouncil();
  const navigate = useNavigate();
  // A phase key, or "live": which moment of the council the room is drawn at.
  const [viewing, setViewing] = useState<string>("live");
  // What a click pinned. While nothing is pinned the dossier follows the room.
  const [pinned, setPinned] = useState<CourtSelection>(null);
  const [recordOpen, setRecordOpen] = useState(false);

  const phases = processLine(detail);
  const picked = viewing === "live" ? undefined : phases.find((phase) => phase.key === viewing);
  const at = picked ?? "live";
  const moment = picked ?? liveMoment(detail);
  const court = buildSessionCourt(detail, at, chairsOf(id));

  // Follow live: whoever is speaking, or the bench while the chair writes.
  const speaking = court.seats.find((seat) => seat.state === "speaking");
  const chairWriting = at === "live" && detail.status === "running" && detail.current_phase === "synthesis";
  const followed: CourtSelection = chairWriting
    ? { kind: "judge" }
    : speaking !== undefined
      ? { kind: "seat", index: speaking.index }
      : null;
  const selection = pinned ?? followed;
  const following = pinned === null && picked === undefined;

  const wingAction =
    detail.status === "running" ? (
      <ConfirmButton
        label="Cancel"
        confirmLabel="Cancel this council"
        variant="ghost"
        intent="stop"
        disabled={cancel.isPending}
        onConfirm={() => cancel.mutate(id)}
      />
    ) : (
      // Back to the courtroom with this council's question, panel and rounds
      // already in it — one press of Convene asks it again.
      <Button
        variant="ghost"
        onClick={() => {
          offerDraft(draftFrom(detail));
          void navigate({ to: "/council" });
        }}
      >
        Ask again
      </Button>
    );

  return (
    <>
      <div className="council-facts">
        <StateBadge domain="council" state={detail.status} />
        <span className="council-phase">{progressOf(detail)}</span>
        <span className="council-phase">
          {detail.rounds_run} of {detail.rounds} {detail.rounds === 1 ? "round" : "rounds"} run
          {detail.stopped_early ? " · stopped early" : ""}
        </span>
        <span className="council-chairman">{chairmanLine(detail)}</span>
        <RelativeTime at={detail.created_at} />
      </div>

      <section className="council-court-page" aria-label="Court">
        <Courtroom
          court={court}
          mode="session"
          selected={selection}
          onSelect={setPinned}
          wingAction={wingAction}
          settle={detail.status === "running" && detail.current_round === 0}
        />
        <Dossier
          mode="session"
          view={detail}
          court={court}
          moment={moment}
          selection={selection}
          following={following}
          onFollowLive={() => {
            setPinned(null);
            setViewing("live");
          }}
        />
        <ProcessLine mode="session" phases={phases} liveKey={momentKey(liveMoment(detail))} viewing={viewing} onView={setViewing} />
      </section>

      {cancel.isError && <CancelRefusal error={cancel.error} />}
      {/* `false` is a success that changed nothing — the council had already
          ended, which is not an error and is not silence either. */}
      {cancel.data?.cancelled === false && (
        <p className="council-note" role="status">
          it had already ended.
        </p>
      )}

      <Panel
        title="Case record"
        aside={
          <Button aria-expanded={recordOpen} onClick={() => setRecordOpen(!recordOpen)}>
            {recordOpen ? "Hide" : "Show"}
          </Button>
        }
      >
        {/* Mounted only open: the columns poll their own run tails, and a shut
            record has no reason to. Before any critique round its one tab,
            "Answers", is where the seats' answers live. */}
        {recordOpen && <CouncilDeliberation key={detail.id} view={detail} running={councilIsAlive(detail.status)} />}
      </Panel>
    </>
  );
}

function DetailError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "there is no council with that id", ...daemonProse(error) }}
      />
    );
  }
  return <ErrorNote>the núcleo did not answer — nothing is known about this council</ErrorNote>;
}

function CancelRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this council was not cancelled</ErrorNote>;
}
