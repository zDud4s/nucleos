import { Fragment, useEffect, useId, useState } from "react";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { canTakeASeat, useAgents, type Agent } from "../data/agents";
import { useAssistantModels, type ModelChoice } from "../data/chats";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useCancelCouncil,
  useCouncil,
  useCouncilOutcome,
  outcomeOf,
  useCouncils,
  useCreateCouncil,
  useCouncilConfig,
  ceilingCalls,
  readCouncilConfig,
  stepsInSequence,
  councilIsAlive,
  seatName,
  type BordaRow,
  type ConfiguredSeat,
  type CouncilConfig,
  type CouncilSummary,
  type CouncilView,
  type RosterOverride,
  type RosterSeat,
  type SeatView,
} from "../data/council";
import { CouncilDeliberation } from "./CouncilRounds";
import { CouncilSynthesis } from "./CouncilSynthesis";
import { clearDraft, draftFrom, offerDraft, peekDraft } from "./council-draft";
import {
  Button,
  ConfirmButton,
  Count,
  Crumb,
  ErrorNote,
  Meter,
  PageHeader,
  Panel,
  Quiet,
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
 * list and the ask bar are the first; an open council replaces both, with a
 * crumb back to the list above its header, so the detail is read on its own.
 *
 * The detail ranks the verdict above the process: the question, then the
 * synthesis, then the leaderboard, then the deliberation that produced them.
 * The order holds however far a council got, and only the synthesis panel
 * changes shape when the chairman never wrote one — a council whose chairman
 * failed still has real answers on it, and hiding them behind the one panel
 * that failed would throw the rest away.
 *
 * Progress is told as a round and a phase — "round 1 · critique" — because a
 * council now runs as many critique rounds as it was asked for, and a fixed
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
          <Composer />

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

/** The helper under the ask bar, when nothing is in the way of convening. */
const COMPOSER_HELPER =
  "Each seat answers on its own, ranks the others blind, and the chair writes one answer. Roster from ~/.nucleos/council.yaml.";

/** PURE: how a configured seat reads in the roster chips. */
function configuredSeatName(seat: ConfiguredSeat): string {
  if ("agent" in seat) return seat.agent;
  return seat.ref ?? seat.kind ?? "default";
}

/**
 * PURE: why Convene cannot be pressed, or `null` when only the question can
 * stop it. The order is the reason: a config not yet read says nothing about
 * the roster, and a roster the daemon refuses makes a half-chosen panel moot —
 * `council::start_with` refuses `NotConfigured` even with a panel chosen.
 * An empty question disables the button without a sentence: the empty field
 * already says so.
 */
function conveneBlocked(
  pending: boolean,
  failed: boolean,
  config: CouncilConfig | null,
  halfChosen: boolean,
): string | null {
  if (pending) return "Reading the council config…";
  if (failed || config === null) return "The council config could not be read.";
  if (!config.configured) {
    return "No roster in ~/.nucleos/council.yaml — add one and restart the núcleo.";
  }
  if (halfChosen) return "Choose every seat, or close the panel.";
  return null;
}

/**
 * The ask bar: one line on the page ground, growing to four while it is being
 * written in, with the roster, the rounds and the ceiling on one row beneath.
 * Not a panel — the question is the page's first act, not a card among cards.
 */
function Composer() {
  /*
   * "Ask again" leaves a draft behind. The initialisers only peek at it and the
   * mount effect drops it, so StrictMode's second initialiser run still sees it
   * and a later visit to the page starts empty.
   */
  const [question, setQuestion] = useState(() => peekDraft()?.question ?? "");
  const [focused, setFocused] = useState(false);
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
  const [choosing, setChoosing] = useState(() => peekDraft() !== null);
  const [chairman, setChairman] = useState<RosterSeat | null>(() => peekDraft()?.chairman ?? null);
  const [members, setMembers] = useState<(RosterSeat | null)[]>(() => peekDraft()?.members ?? [null]);
  const create = useCreateCouncil();
  const navigate = useNavigate();
  const reasonId = useId();
  // One cheap read on every visit, unlike the catalogues above: the bounds it
  // carries are what a council convened from here will run under, and saying
  // so before the question is asked is the point of the endpoint. Read through
  // the shape guard, because `apiFetch` casts whatever a `200` carried.
  const configQuery = useCouncilConfig();
  const config = configQuery.data === undefined ? null : readCouncilConfig(configQuery.data);

  /**
   * The rounds somebody picked, or `null` while they have picked none.
   *
   * `null` and not the config's default copied into state: the config answers
   * after the form mounts, so a copy taken at mount would be the wrong number,
   * and "untouched" has to stay distinguishable from "chose the default" for
   * the request to keep leaving the key off.
   */
  const [pickedRounds, setPickedRounds] = useState<number | null>(() => peekDraft()?.rounds ?? null);
  /** A role per member row, keyed by the row's index. Absent means "no role". */
  const [roles, setRoles] = useState<Record<number, string>>(() => peekDraft()?.roles ?? {});
  useEffect(() => clearDraft(), []);

  const roster = rosterFrom(chairman, members);
  const halfChosen = choosing && roster === null;
  const defaultRounds = config?.default_rounds;
  const rounds = pickedRounds ?? defaultRounds;
  const estimate = estimateMembers(choosing, members, config?.default_roster?.members.length);
  const reason = conveneBlocked(
    configQuery.data === undefined && configQuery.isPending,
    configQuery.isError,
    config,
    halfChosen,
  );
  const disabled = question.trim() === "" || create.isPending || reason !== null;

  // The button and ctrl+enter share this path, so the keyboard can never send
  // what the disabled button would have refused.
  const submit = () => {
    if (disabled) return;
    create.mutate(
      // `undefined` and not `null`: the key is left off the request
      // entirely when nothing is being overridden. See `data/council.ts`.
      {
        question: question.trim(),
        roster: choosing && roster !== null ? roster : undefined,
        // Only a departure from the file's default travels; the default
        // itself is the daemon's to apply, as it is for every request
        // that never had this control.
        rounds: rounds !== undefined && rounds !== defaultRounds ? rounds : undefined,
        // Roles belong to the chosen panel's rows, so they travel with it
        // and never on their own against a roster this form did not draw.
        roles: choosing && roster !== null ? rolesBody(roles, members.length) : undefined,
      },
      {
        onSuccess: (result) => {
          setQuestion("");
          void navigate({ to: `/council/${result.id}` });
        },
      },
    );
  };

  return (
    <form
      className="council-ask"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <div className="council-field">
        <textarea
          rows={focused || question !== "" ? 4 : 1}
          aria-label="Question"
          placeholder="Ask the council…"
          value={question}
          onChange={(event) => setQuestion(event.target.value)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
              event.preventDefault();
              submit();
            }
          }}
        />
      </div>

      <div className="council-ask-bar">
        <Button aria-expanded={choosing} onClick={() => setChoosing(!choosing)}>
          {choosing ? (
            `Chosen panel · ${members.length} ${members.length === 1 ? "seat" : "seats"}`
          ) : (
            <RosterChips config={config} />
          )}
        </Button>

        {config !== null && (
          <div className="ui-switch" role="group" aria-label="Rounds">
            {roundChoices(config.max_rounds).map((choice) => (
              <button
                key={choice}
                type="button"
                className="ui-switch-seg"
                aria-pressed={choice === rounds}
                onClick={() => setPickedRounds(choice)}
              >
                {choice}
              </button>
            ))}
          </div>
        )}

        {/* One template string, so the line is one text node a reader and a
            test both find whole. A ceiling, not a forecast: a council that
            stops early spends less. */}
        {estimate !== undefined && rounds !== undefined && (
          <span className="council-ceiling">
            {`≤ ${ceilingCalls(estimate, rounds)} calls · ${stepsInSequence(rounds)} steps in sequence`}
          </span>
        )}

        <Button
          type="submit"
          intent="go"
          disabled={disabled}
          aria-describedby={reason !== null ? reasonId : undefined}
        >
          Convene
        </Button>
      </div>

      {reason !== null ? (
        <p className="ui-field-helper council-ask-helper" id={reasonId}>
          {reason}
        </p>
      ) : (
        <p className="ui-field-helper council-ask-helper">{COMPOSER_HELPER}</p>
      )}

      {choosing && (
        <RosterPicker
          chairman={chairman}
          members={members}
          roles={roles}
          roleChoices={config?.roles ?? []}
          onChairman={setChairman}
          onMembers={setMembers}
          onRoles={setRoles}
        />
      )}

      {create.isError && <ConveneRefusal error={create.error} />}
    </form>
  );
}

/**
 * The configured roster as bare-text chips: "Chair: X · Seats: A, B, C". One
 * run of text with spans inside, so the button's name is the whole sentence.
 */
function RosterChips({ config }: { config: CouncilConfig | null }) {
  const roster = config?.default_roster ?? null;
  const chair = roster === null ? "default" : configuredSeatName(roster.chairman);
  const seats = roster === null ? ["default"] : roster.members.map(configuredSeatName);
  return (
    <>
      {"Chair: "}
      <span className="council-chip">{chair}</span>
      {" · Seats: "}
      {seats.map((seat, at) => (
        <Fragment key={at}>
          {at > 0 && ", "}
          <span className="council-chip">{seat}</span>
        </Fragment>
      ))}
    </>
  );
}

/** PURE: 1..max, or nothing while the config has not answered. */
function roundChoices(max: number | undefined): number[] {
  if (max === undefined) return [];
  return Array.from({ length: max }, (_, at) => at + 1);
}

/**
 * PURE: how many members the estimate counts, or `undefined` when nothing says.
 *
 * The chosen rows once the picker is open and at least one row is filled — the
 * panel on screen is the one that would run — and otherwise the configured
 * roster the request would fall back to. With neither there is no honest
 * number, and the line is left off rather than guessed.
 */
function estimateMembers(
  choosing: boolean,
  members: (RosterSeat | null)[],
  configured: number | undefined,
): number | undefined {
  if (choosing) {
    const chosen = members.filter((member) => member !== null).length;
    if (chosen > 0) return chosen;
  }
  return configured;
}

/**
 * PURE: the `roles` key as the daemon reads it — the row's index as a string —
 * or `undefined` when no row plays one, so the key stays off the request.
 * Rows past the panel's end are ignored: a role is only ever a row's.
 */
function rolesBody(roles: Record<number, string>, rows: number): Record<string, string> | undefined {
  const body: Record<string, string> = {};
  for (const [index, role] of Object.entries(roles)) {
    if (role !== "" && Number(index) < rows) body[index] = role;
  }
  return Object.keys(body).length > 0 ? body : undefined;
}

/**
 * PURE: the roles after row `removed` comes out. The rows below it move up one,
 * and their roles have to move with them or a role would land on its neighbour.
 */
function rolesWithout(roles: Record<number, string>, removed: number): Record<number, string> {
  const next: Record<number, string> = {};
  for (const [key, role] of Object.entries(roles)) {
    const index = Number(key);
    if (index < removed) next[index] = role;
    else if (index > removed) next[index - 1] = role;
  }
  return next;
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
  roles,
  roleChoices,
  onChairman,
  onMembers,
  onRoles,
}: {
  chairman: RosterSeat | null;
  members: (RosterSeat | null)[];
  roles: Record<number, string>;
  roleChoices: string[];
  onChairman: (seat: RosterSeat | null) => void;
  onMembers: (seats: (RosterSeat | null)[]) => void;
  onRoles: (roles: Record<number, string>) => void;
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
    <div className="council-roster ui-panel-inset">
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
              label={`Seat ${index + 1}`}
              seat={member}
              agents={seatable}
              models={choices}
              onChange={(seat) => onMembers(members.map((old, at) => (at === index ? seat : old)))}
            />
            {/* The chairman has no role: it synthesises, it does not argue. */}
            <label className="council-field">
              <span>Role</span>
              <select
                className="council-select"
                aria-label={`Role for seat ${index + 1}`}
                value={roles[index] ?? ""}
                onChange={(event) => onRoles({ ...roles, [index]: event.target.value })}
              >
                <option value="">no role</option>
                {roleChoices.map((role) => (
                  <option key={role} value={role}>
                    {role}
                  </option>
                ))}
              </select>
            </label>
            {/* The last row does not come out: `council::start` refuses a roster
                with no members, so an empty panel would be a refusal rather
                than a way back to the file. Closing the panel is that. */}
            <Button
              variant="quiet"
              aria-label={`Remove seat ${index + 1}`}
              disabled={members.length === 1}
              onClick={() => {
                onMembers(members.filter((_, at) => at !== index));
                onRoles(rolesWithout(roles, index));
              }}
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
          <p className="council-note">Eight seats at most.</p>
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
 * The rows are labelled from 1, one ahead of the daemon: `config::seat_name`
 * counts the chairman, then seat 0 upward, so a `400` naming "seat 2" names
 * the row this page labels "Seat 3". The values and the `roles` keys stay
 * 0-based — only the label a person reads is shifted.
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
  const cancel = useCancelCouncil();
  const navigate = useNavigate();
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
              variant="ghost"
              intent="stop"
              disabled={cancel.isPending}
              onConfirm={() => cancel.mutate(id)}
            />
          ) : (
            // Back to the ask bar with this council's question, panel and
            // rounds already in it — one press of Convene asks it again.
            <Button
              variant="ghost"
              onClick={() => {
                offerDraft(draftFrom(detail));
                void navigate({ to: "/council" });
              }}
            >
              Ask again
            </Button>
          )
        }
      >
        <p className="council-question">{detail.question}</p>
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
        {cancel.isError && <CancelRefusal error={cancel.error} />}
        {/* `false` is a success that changed nothing — the council had already
            ended, which is not an error and is not silence either. */}
        {cancel.data?.cancelled === false && (
          <p className="council-note" role="status">
            it had already ended.
          </p>
        )}
      </Panel>

      {/* The verdict before the process: what the chairman concluded, then how
          the seats ranked each other, then the deliberation itself. */}
      <CouncilSynthesis view={detail} />
      <Leaderboard leaderboard={detail.leaderboard} seats={detail.seats} />
      {/* Always drawn: before any critique round its one tab, "Answers", is
          where the seats' answers live, and it is the only place they do. */}
      <CouncilDeliberation view={detail} running={councilIsAlive(detail.status)} />
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

/* ------------------------------------------------------------- leaderboard -- */

/**
 * The last critique round's Borda leaderboard, in the order the daemon served
 * it. Each seat by name — "seat 1" is a number the reader then has to carry up
 * to the seat grid to decode — with its score drawn as a bar, the figure, and
 * the ballots behind it: one ballot and four are not the same claim.
 *
 * The bar's ceiling is the best score possible. `tally.rs` scores a seat as its
 * per-ballot normalised mean, so that is 1; councils from before the
 * normalisation scored above it, and there the top score is the ceiling.
 */
function Leaderboard({ leaderboard, seats }: { leaderboard: BordaRow[]; seats: SeatView[] }) {
  const max = Math.max(1, ...leaderboard.map((entry) => entry.score));
  return (
    <Panel title="Leaderboard">
      {leaderboard.length === 0 ? (
        <Quiet says="No ranking — fewer than two answers to rank." />
      ) : (
        /* A column read by scanning down it rather than picked out of, so it is
           `Rows` and not a stack of boxes; the four parts of a ranking line up
           as columns across the rows. */
        <Rows label="Leaderboard">
          {leaderboard.map((entry) => {
            const seat = seats.find((candidate) => candidate.seat_idx === entry.seat_idx);
            // A row naming a seat the view does not carry is the daemon's
            // inconsistency, said as such rather than as a bare number.
            const name = seat === undefined ? `an unrecorded seat (#${entry.seat_idx})` : seatName(seat);
            return (
              <Row layout="line" className="council-leaderboard-row" key={entry.seat_idx}>
                <span className="council-leaderboard-seat">{name}</span>
                {/* A div, because `Meter` draws a paragraph. */}
                <div className="council-leaderboard-bar">
                  <Meter
                    label={name}
                    value={entry.score}
                    ceiling={max}
                    tone="quantity"
                    head={false}
                    format={(value) => value.toFixed(2)}
                  />
                </div>
                <span className="council-leaderboard-rank">{entry.score.toFixed(2)}</span>
                <span className="council-leaderboard-n">{`${entry.n} ${entry.n === 1 ? "ballot" : "ballots"}`}</span>
              </Row>
            );
          })}
        </Rows>
      )}
    </Panel>
  );
}
