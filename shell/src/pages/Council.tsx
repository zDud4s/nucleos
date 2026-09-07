import { useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { canTakeASeat, useAgents, type Agent } from "../data/agents";
import { useAssistantModels, type ModelChoice } from "../data/chats";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCancelCouncil,
  useCouncil,
  useCouncils,
  useCreateCouncil,
  type CouncilSummary,
  type CouncilView,
  type LeaderboardEntry,
  type RosterOverride,
  type RosterSeat,
  type SeatView,
} from "../data/council";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./council.css";

/**
 * Council — one component serving `/council` and `/council/$councilId`, the
 * `Projects` pattern: a list that is always on screen, with the detail added
 * below it once something is selected rather than replacing it.
 *
 * Three phases, always drawn in the same order regardless of how far a council
 * got: seats (phase 1 and 2 together, one card per seat) and the leaderboard
 * (phase 2's output) render whenever there are seats at all, and only the
 * synthesis panel (phase 3) changes shape when the chairman never wrote one —
 * a council whose chairman failed still has two phases worth of real answers
 * on it, and hiding them behind the one panel that failed would throw the rest
 * away.
 */
export function Council() {
  const params = useParams({ strict: false }) as { councilId?: string };
  const councilId = params.councilId ?? null;

  const councils = useCouncils();
  const rows = councils.data ?? [];
  const stale = councils.isError && councils.data !== undefined;

  return (
    <>
      <PageHeader title="Council" headline={headlineFor(rows, councils.data !== undefined)} />

      <ConveneForm />

      {stale && <StaleNote dataUpdatedAt={councils.dataUpdatedAt} />}
      {councils.isError && councils.data === undefined && <ListError error={councils.error} />}

      <CouncilList rows={rows} answered={councils.data !== undefined} selected={councilId} />

      {councilId === null && (
        <Teach title="Choose a council">
          <p>
            Pick a question from the list, or convene a new one above. Every seat answers on its
            own, ranks the others blind, and a chairman writes a synthesis — three phases, in
            order, and this page shows all three whichever one a council has reached.
          </p>
        </Teach>
      )}

      {councilId !== null && <CouncilDetail key={councilId} id={councilId} />}
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

/**
 * The ceiling on a roster's MEMBERS — `config::MAX_COUNCIL_SEATS`.
 *
 * Eight, and it counts the members alone: `council::start` compares
 * `members.len()` against it and resolves the chairman apart from them, so a
 * panel of eight plus a chairman is accepted and nine members is a `400`. The
 * add button switches itself off at the eighth rather than letting somebody
 * assemble a roster that cannot be convened and only find out on submit.
 *
 * A copy of a núcleo constant, which this codebase normally refuses. It is here
 * because the alternative is not a shared constant — no route serves this
 * number — but a button that stays enabled and a refusal after the fact. Eight
 * has not moved since the Python orchestrator this pillar was ported out of,
 * and `config.rs` says why a file may lower the fan-out and may not raise it.
 */
const MAX_COUNCIL_SEATS = 8;

/**
 * The two prefixes one seat picker's `<option>` values carry.
 *
 * A seat is filled by an agent OR by a model and never both — `resolve_seat`
 * refuses both-at-once before anything is spent — so the form offers ONE
 * control per seat with both catalogues inside it, rather than two controls and
 * a rule about which of them wins. The exclusive choice becomes the widget's
 * shape instead of a validation somebody has to remember to write.
 *
 * Prefixed because an agent id and a model id are both free text and could
 * collide; the prefix is what says which catalogue a value came out of.
 */
const AGENT_CHOICE = "agent:";
const MODEL_CHOICE = "model:";

/** PURE: the option value standing for a seat already chosen. The inverse of `seatFromChoice`. */
function choiceOf(seat: RosterSeat | null): string {
  if (seat === null) return "";
  return "agent" in seat ? `${AGENT_CHOICE}${seat.agent}` : `${MODEL_CHOICE}${seat.ref}`;
}

/**
 * PURE: the seat an option value stands for, resolved against the model menu.
 *
 * The menu is needed because `SeatSpec` wants a `kind` the option value does
 * not carry, and `ModelChoice.brain` is the only place this app knows a model's
 * locality. `local` is the one brain a seat may call local: `cloud` and
 * `openrouter` both answer from somebody else's machine, which is exactly what
 * `SeatKind::Cloud` means, and mapping `openrouter` to `local` would tell the
 * daemon to run a hosted model through `local_agent.rs`.
 *
 * A model the menu no longer lists resolves to `null` — the seat goes back to
 * unchosen rather than travelling as a `ref` nothing can serve.
 */
function seatFromChoice(value: string, models: ModelChoice[]): RosterSeat | null {
  if (value.startsWith(AGENT_CHOICE)) return { agent: value.slice(AGENT_CHOICE.length) };
  if (!value.startsWith(MODEL_CHOICE)) return null;
  const id = value.slice(MODEL_CHOICE.length);
  const model = models.find((candidate) => candidate.id === id);
  if (model === undefined) return null;
  return { kind: model.brain === "local" ? "local" : "cloud", ref: model.id };
}

/**
 * PURE: the override these choices make, or `null` while they do not make one.
 *
 * Every row has to be chosen. An unchosen one cannot be encoded at all — there
 * is no `SeatSpec` meaning "nobody" — and quietly dropping it would convene a
 * panel one seat smaller than the panel on screen. So the Convene button waits
 * instead, and the empty row stays there to be filled or removed.
 */
function rosterFrom(
  chairman: RosterSeat | null,
  members: (RosterSeat | null)[],
): RosterOverride | null {
  // An empty list is its own refusal in `council::start`, and is reachable here
  // only by removing every row — which the Remove buttons do not allow.
  if (chairman === null || members.length === 0) return null;
  const chosen: RosterSeat[] = [];
  for (const member of members) {
    if (member === null) return null;
    chosen.push(member);
  }
  return { chairman, members: chosen };
}

function ConveneForm() {
  const [question, setQuestion] = useState("");
  /**
   * Shut, and shut is the whole point.
   *
   * A closed panel sends `{ question }` and nothing else — the request this
   * page made for its entire life before the control existed — and asks the
   * daemon nothing extra either: the two catalogues are read inside
   * `RosterPicker`, which is not mounted until somebody opens it. Adding a
   * control should not add two requests to every visit of a page that is
   * usually used without it.
   */
  const [choosing, setChoosing] = useState(false);
  const [chairman, setChairman] = useState<RosterSeat | null>(null);
  const [members, setMembers] = useState<(RosterSeat | null)[]>([null]);
  const create = useCreateCouncil();
  const navigate = useNavigate();

  const roster = rosterFrom(chairman, members);
  const halfChosen = choosing && roster === null;

  return (
    <Panel title="Convene a council">
      <p className="council-note">
        One question, put to every seat in <code>~/.nucleos/council.yaml</code>. Each seat answers
        on its own, ranks the others blind, and a chairman writes a synthesis. A panel chosen below
        stands in for that roster for this one question, and never rewrites the file.
      </p>
      <form
        className="council-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (question.trim() === "" || create.isPending || halfChosen) return;
          create.mutate(
            // `undefined` and not `null`: the key is left off the request
            // entirely when nothing is being overridden. See `data/council.ts`.
            { question: question.trim(), roster: choosing && roster !== null ? roster : undefined },
            {
              onSuccess: (result) => {
                setQuestion("");
                void navigate({ to: `/council/${result.id}` });
              },
            },
          );
        }}
      >
        <label className="council-field">
          <span>Question</span>
          <textarea
            rows={3}
            aria-label="Question"
            value={question}
            onChange={(event) => setQuestion(event.target.value)}
          />
        </label>

        <label className="council-roster-open">
          <input
            type="checkbox"
            checked={choosing}
            onChange={(event) => setChoosing(event.target.checked)}
          />
          <span>Put this question to a chosen panel</span>
        </label>

        {choosing && (
          <RosterPicker
            chairman={chairman}
            members={members}
            onChairman={setChairman}
            onMembers={setMembers}
          />
        )}

        <Button
          type="submit"
          intent="go"
          disabled={question.trim() === "" || create.isPending || halfChosen}
        >
          Convene
        </Button>
      </form>
      {create.isError && <ConveneRefusal error={create.error} />}
    </Panel>
  );
}

/**
 * Who sits on the panel for this question.
 *
 * Mounted only while the control is open, which is what keeps `/agents` and
 * `/assistant/models` off the page for everybody using the configured roster.
 * The state lives above this component, so closing the control and opening it
 * again does not discard a panel somebody half-assembled.
 */
function RosterPicker({
  chairman,
  members,
  onChairman,
  onMembers,
}: {
  chairman: RosterSeat | null;
  members: (RosterSeat | null)[];
  onChairman: (seat: RosterSeat | null) => void;
  onMembers: (seats: (RosterSeat | null)[]) => void;
}) {
  const agents = useAgents();
  const models = useAssistantModels();

  // `canTakeASeat` and not a rule written here: it exists to answer this exact
  // question and is already the shell's reading of `council.rs:849` — a seat's
  // row records the model that answered, `NOT NULL`, so an agent naming no
  // model is a refusal waiting to happen. Offering it would be offering a 400.
  const seatable = (agents.data ?? []).filter(canTakeASeat);
  const choices = models.data?.choices ?? [];
  const full = members.length >= MAX_COUNCIL_SEATS;

  return (
    <div className="council-roster">
      <SeatPicker
        label="Chairman"
        seat={chairman}
        agents={seatable}
        models={choices}
        onChange={onChairman}
      />
      <ul className="council-roster-seats" aria-label="Panel">
        {members.map((member, index) => (
          // Keyed by position because a row has no identity of its own — an
          // unchosen one is `null`, and two rows may legitimately hold the
          // same seat.
          <li className="council-roster-seat" key={index}>
            <SeatPicker
              label={`Seat ${index}`}
              seat={member}
              agents={seatable}
              models={choices}
              onChange={(seat) => onMembers(members.map((old, at) => (at === index ? seat : old)))}
            />
            {/* The last row does not come out: `council::start` refuses a roster
                with no members, so an empty panel would be a refusal rather
                than a way back to the file. Unticking the box is that. */}
            <Button
              variant="quiet"
              aria-label={`Remove seat ${index}`}
              disabled={members.length === 1}
              onClick={() => onMembers(members.filter((_, at) => at !== index))}
            >
              Remove
            </Button>
          </li>
        ))}
      </ul>
      <div className="council-roster-actions">
        <Button disabled={full} onClick={() => onMembers([...members, null])}>
          Add a seat
        </Button>
        {full && (
          <p className="council-note">
            eight is the ceiling — a ninth seat is a refusal, not a larger council.
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * One seat, chosen from both catalogues at once.
 *
 * The idiom is `team/Charter.tsx`'s director picker: a plain `<select>` whose
 * first option says what to do rather than quietly being the answer. Two
 * `<optgroup>`s because an agent and a model are different kinds of choice —
 * an agent brings a prompt and a persona, a model is only a model — and a flat
 * list would present them as one menu of interchangeable names.
 *
 * The rows are labelled the way the daemon labels them in a refusal
 * (`config::seat_name`: the chairman, then seat 0 upward), so a `400` naming
 * "seat 2" names a row that is on the screen.
 */
function SeatPicker({
  label,
  seat,
  agents,
  models,
  onChange,
}: {
  label: string;
  seat: RosterSeat | null;
  agents: Agent[];
  models: ModelChoice[];
  onChange: (seat: RosterSeat | null) => void;
}) {
  return (
    <label className="council-field">
      <span>{label}</span>
      <select
        className="council-select"
        aria-label={label}
        value={choiceOf(seat)}
        onChange={(event) => onChange(seatFromChoice(event.target.value, models))}
      >
        <option value="">choose an agent or a model</option>
        {agents.length > 0 && (
          <optgroup label="Agents">
            {agents.map((agent) => (
              <option key={agent.id} value={`${AGENT_CHOICE}${agent.id}`}>
                {agent.name}
              </option>
            ))}
          </optgroup>
        )}
        {models.length > 0 && (
          <optgroup label="Models">
            {models.map((model) => (
              <option key={model.id} value={`${MODEL_CHOICE}${model.id}`}>
                {model.label}
              </option>
            ))}
          </optgroup>
        )}
      </select>
    </label>
  );
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

function CouncilList({
  rows,
  answered,
  selected,
}: {
  rows: CouncilSummary[];
  answered: boolean;
  selected: string | null;
}) {
  return (
    <Panel title="Councils" aside={<Count n={answered ? rows.length : undefined} />}>
      {!answered && <p className="council-loading">reading the councils…</p>}
      {answered && rows.length === 0 && <p className="council-empty">no council has been convened yet.</p>}
      {rows.length > 0 && (
        <ul className="council-list" aria-label="Councils">
          {rows.map((row) => (
            <CouncilRow key={row.id} row={row} active={row.id === selected} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function CouncilRow({ row, active }: { row: CouncilSummary; active: boolean }) {
  return (
    <li className={active ? "council-row council-row-active" : "council-row"}>
      <Link className="council-row-link" to={`/council/${row.id}`} aria-current={active ? "page" : undefined}>
        <span className="council-row-question">{row.question}</span>
        <StateBadge domain="council" state={row.status} />
        <span className="council-row-phase">phase {row.stage} of 3</span>
        <RelativeTime at={row.created_at} />
      </Link>
    </li>
  );
}

/* ----------------------------------------------------------------- detail -- */

function CouncilDetail({ id }: { id: string }) {
  const council = useCouncil(id);
  const cancel = useCancelCouncil();
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

  return (
    <>
      <Panel
        title="This council"
        aside={
          detail.status === "running" ? (
            <ConfirmButton
              label="Cancel"
              confirmLabel="Cancel this council"
              intent="stop"
              disabled={cancel.isPending}
              onConfirm={() => cancel.mutate(id)}
            />
          ) : undefined
        }
      >
        <p className="council-question">{detail.question}</p>
        <div className="council-facts">
          <StateBadge domain="council" state={detail.status} />
          <span className="council-phase">phase {detail.stage} of 3</span>
          <span className="council-chairman">{chairmanLine(detail)}</span>
          <RelativeTime at={detail.created_at} />
        </div>
        {cancel.isError && <CancelRefusal error={cancel.error} />}
        {/* `false` is a success that changed nothing — the council had already
            ended, which is not an error and is not silence either. */}
        {cancel.data?.cancelled === false && (
          <p className="council-note" role="status">
            it had already ended.
          </p>
        )}
      </Panel>

      <SeatGrid seats={detail.seats} />
      <Leaderboard leaderboard={detail.leaderboard} />
      <Synthesis synthesis={detail.synthesis} error={detail.error} />
    </>
  );
}

/**
 * Who chaired, and on what.
 *
 * This panel never said. The synthesis below it is one seat's writing, and a
 * reader who disagrees with it has no way to ask which of the roster wrote it.
 * `chairman_ref` is printed whether or not an agent chaired, because the model
 * is the fact that survives — an agent can be renamed or deleted, and the row
 * keeps the model that answered on purpose (`0065_council.sql`).
 *
 * A deleted chairman falls back to its id rather than to nothing: an id is
 * ugly and is still an answer to "who".
 */
function chairmanLine(view: CouncilView): string {
  const named = view.chairman_agent_name ?? view.chairman_agent_id;
  if (named === null) return `chaired by ${view.chairman_ref}`;
  return `chaired by ${named} on ${view.chairman_ref}`;
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

/* ------------------------------------------------------------------- seats -- */

function SeatGrid({ seats }: { seats: SeatView[] }) {
  return (
    <Panel title="Seats" aside={<Count n={seats.length} />}>
      {seats.length === 0 ? (
        <p className="council-empty">no seat has been recorded for this council yet.</p>
      ) : (
        <ul className="council-seats" aria-label="Seats">
          {seats.map((seat) => (
            <SeatCard key={seat.seat_idx} seat={seat} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/**
 * What this seat is called out loud.
 *
 * An agent's name wins the title, and the model stays underneath it in
 * `.council-seat-ref`: *who* answered and *what* ran are different facts, and
 * the second is the one you reach for when the answer is bad. A seat the
 * roster named by model has no name of its own, so its kind is still the title
 * — that case is unchanged and is not the lesser one.
 *
 * The agent's id stands in when the name is gone. `agent_name` is read from
 * the catalogue as the view is built, so a `null` beside a set `agent_id`
 * means the agent has been deleted since it answered — a real state the seat
 * says out loud rather than papering over with a blank line.
 */
function seatTitle(seat: SeatView): string {
  if (seat.agent_name !== null) return seat.agent_name;
  if (seat.agent_id !== null) return seat.agent_id;
  const trimmed = seat.kind.trim();
  return trimmed === "" ? "unnamed seat" : trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
}

/**
 * What a seat's answer reads as.
 *
 * `answer === null` is ambiguous on its own — nothing written yet, or a
 * transcript that has since been pruned — and the two must not read the same.
 * `stage1_status === "ok"` with no answer is the pruned case: the seat did
 * answer, and the text is simply gone now.
 */
function answerText(seat: SeatView): string {
  if (seat.answer !== null) return seat.answer;
  if (seat.stage1_status === "ok") return "answered — the text has expired";
  return "no answer recorded";
}

function SeatCard({ seat }: { seat: SeatView }) {
  const abstained = seat.stage2_status === "ok" && seat.rankings.length === 0;
  // An agent that answered and is no longer in the catalogue. Told apart from a
  // model-named seat by `agent_id`, which the row keeps forever.
  const agentIsGone = seat.agent_id !== null && seat.agent_name === null;

  return (
    <li className="council-seat">
      <div className="council-seat-head">
        <span
          className={
            seat.agent_id === null ? "council-seat-name" : "council-seat-name council-seat-agent"
          }
        >
          {seatTitle(seat)}
        </span>
        <span className="council-seat-idx">seat {seat.seat_idx}</span>
      </div>
      <p className="council-seat-ref">{seat.ref}</p>
      {agentIsGone && (
        <p className="council-seat-gone">this agent is no longer in the catalogue</p>
      )}

      <div className="council-seat-stage">
        <span className="council-seat-stage-label">stage 1</span>
        <StateBadge domain="council_seat" state={seat.stage1_status} />
      </div>
      {seat.stage1_error !== null && (
        <p className="council-seat-error" role="alert">
          {seat.stage1_error}
        </p>
      )}
      <p className="council-seat-answer">{answerText(seat)}</p>

      <div className="council-seat-stage">
        <span className="council-seat-stage-label">stage 2</span>
        <StateBadge domain="council_seat" state={seat.stage2_status} />
      </div>
      {seat.stage2_error !== null && (
        <p className="council-seat-error" role="alert">
          {seat.stage2_error}
        </p>
      )}
      {/* A blank vote is a valid outcome, not a failure — the seat answered ok
          and simply ranked nobody. */}
      {abstained && <p className="council-seat-abstained">abstained</p>}
    </li>
  );
}

/* ------------------------------------------------------------- leaderboard -- */

function Leaderboard({ leaderboard }: { leaderboard: LeaderboardEntry[] }) {
  return (
    <Panel title="Leaderboard">
      {leaderboard.length === 0 ? (
        <p className="council-note">
          Fewer than two seats have a valid answer to rank, so there is nothing to show here —
          that does not stop the chairman from writing a synthesis.
        </p>
      ) : (
        <ul className="council-leaderboard" aria-label="Leaderboard">
          {leaderboard.map((entry) => (
            <li className="council-leaderboard-row" key={entry.seat_idx}>
              <span className="council-leaderboard-seat">seat {entry.seat_idx}</span>
              <span className="council-leaderboard-rank">avg rank {entry.avg_rank.toFixed(2)}</span>
              {/* n travels with the average always: one vote and five votes are
                  not the same claim, and dropping this would present them as one. */}
              <span className="council-leaderboard-n">n = {entry.n}</span>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

/* --------------------------------------------------------------- synthesis -- */

/**
 * Phase 3's output — or the fact that the chairman never produced one.
 *
 * A `null` synthesis with `error` set is not a reason to hide phases 1 and 2:
 * the seats and the leaderboard above this panel are real answers regardless
 * of what the chairman did with them, and only this one panel changes shape.
 */
function Synthesis({ synthesis, error }: { synthesis: string | null; error: string | null }) {
  if (synthesis !== null) {
    return (
      <Panel title="Synthesis">
        <p className="council-synthesis">{synthesis}</p>
      </Panel>
    );
  }
  if (error !== null) {
    return (
      <Panel title="Synthesis">
        <p className="council-chairman-failed" role="alert">
          the chairman failed to write a synthesis: {error}
        </p>
      </Panel>
    );
  }
  return (
    <Panel title="Synthesis">
      <p className="council-note">no synthesis yet.</p>
    </Panel>
  );
}

/* ---------------------------------------------------------------- shared -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="council-count">{n}</span>;
}
