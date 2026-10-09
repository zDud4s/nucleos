// §spec alcada-por-equipa
import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useAgents } from "../data/agents";
import { ContextRefs } from "../context/ContextRefs";
import { ScopedMemory } from "../brain/knowledge/ScopedMemory";
import { ScopedTools } from "../brain/knowledge/ScopedTools";
import { isApiRefusal } from "../data/client";
import {
  GRANTABLE_ACTIONS,
  GRANT_MODES,
  useCreateTeam,
  useTeam,
  useTeams,
  useUpdateTeam,
  type TeamGrant,
  type TeamRequest,
  type TeamRun,
  type TeamView,
} from "../data/teams";
import {
  Button,
  ErrorNote,
  LimitChip,
  Meter,
  Panel,
  Quiet,
  RefusalNote,
  Who,
  usd,
} from "../ui";
import { detectDrift, snapshotFromView, type Drift, type DriftField } from "./drift";
import { daemonProse } from "./prose";

/**
 * `Charter` — what a department **is**, in one form under one Save.
 *
 * All eleven fields live here and nowhere else, because `PUT /teams/{id}` is a
 * full replace: `replace_roster` does a `DELETE` and re-inserts, and the grants
 * go the same way (`core/src/team.rs:427`). A second form that owned half of
 * them would wipe the other half every time it saved. There is no `id` field —
 * the daemon slugs one from the name, and renaming never changes it.
 *
 * Five sections rather than eleven boxes in a column, because the eleven are not
 * one list: identity, leadership, staff, powers and limits are five different
 * questions, and the limits split again into the two kinds §2.2 of the design
 * separates — a ceiling something occupies gets a bar, a rule applied to each
 * task gets a chip.
 *
 * ## The guard
 *
 * A department can grow its own roster while this form is open: approving a
 * recruitment on the Decisions tab inserts straight into `team_members`
 * (`core/src/team.rs:1749`). Seeded at 09:41, hired at 09:52, saved at 09:58 —
 * and the save re-sends the four members it was seeded with, `replace_roster`
 * deletes five and re-inserts four, and the new hire is gone with nothing on
 * screen having gone wrong.
 *
 * So the form re-reads the department at submit and compares it with what it was
 * seeded with, raising only fields that changed underneath something nobody was
 * editing (`drift.ts`). Three answers, and none of them is saving quietly.
 *
 * Re-seeding on every poll instead would be the obvious fix and the wrong one:
 * it overwrites a half-typed edit, which is why the seed-once guard exists in
 * the first place.
 */

/* --------------------------------------------------------------- state -- */

/**
 * The form's own shape, with ceilings as strings.
 *
 * A half-typed number is a string, and coercing it while somebody types is how
 * a field fights back — the parse happens once, on the way out. The field names
 * are deliberately the same as `DriftField`'s, so the guard's answer maps onto
 * this without a translation table nobody would keep in step.
 */
export interface TeamFormState {
  name: string;
  mission: string;
  directorAgentId: string;
  maxRounds: string;
  maxParallel: string;
  budgetUsd: string;
  maxOpenActions: string;
  maxLiveRuns: string;
  members: string[];
  grants: TeamGrant[];
}

export function emptyTeamForm(): TeamFormState {
  return {
    name: "",
    mission: "",
    directorAgentId: "",
    maxRounds: "3",
    maxParallel: "2",
    budgetUsd: "",
    maxOpenActions: "5",
    maxLiveRuns: "1",
    members: [],
    grants: [],
  };
}

export function teamFormFromView(team: TeamView): TeamFormState {
  return {
    name: team.name,
    mission: team.mission,
    directorAgentId: team.director_agent_id,
    maxRounds: String(team.max_rounds),
    maxParallel: String(team.max_parallel),
    budgetUsd: team.budget_usd === null ? "" : String(team.budget_usd),
    maxOpenActions: String(team.max_open_actions),
    maxLiveRuns: String(team.max_live_runs),
    members: team.members,
    grants: team.grants,
  };
}

/** A blank ceiling box is `null` — no ceiling — never `0`; a typed `0` is a real ceiling of zero. */
export function parseCeiling(raw: string): number | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

export function teamRequestFromForm(form: TeamFormState): TeamRequest {
  return {
    name: form.name.trim(),
    mission: form.mission.trim(),
    director_agent_id: form.directorAgentId,
    max_rounds: Number(form.maxRounds),
    max_parallel: Number(form.maxParallel),
    budget_usd: parseCeiling(form.budgetUsd),
    max_open_actions: Number(form.maxOpenActions),
    max_live_runs: Number(form.maxLiveRuns),
    members: form.members,
    grants: form.grants,
  };
}

/**
 * The first of the guard's three answers: keep what arrived, keep what was typed.
 *
 * Only the drifted fields are taken from the daemon's copy, so a roster that
 * grew by one comes back with the new hire in it while every field the person
 * was working on stays exactly as they left it.
 *
 * Exported and pure, so the answer that matters most can be asserted without
 * mounting a form.
 */
export function takeTheirs(form: TeamFormState, fresh: TeamView, drifted: readonly Drift[]): TeamFormState {
  const incoming = teamFormFromView(fresh);
  const merged: TeamFormState = { ...form };
  for (const { field } of drifted) {
    switch (field) {
      case "members":
        merged.members = incoming.members;
        break;
      case "grants":
        merged.grants = incoming.grants;
        break;
      default:
        merged[field] = incoming[field];
        break;
    }
  }
  return merged;
}

/* -------------------------------------------------------------- charter -- */

export interface CharterProps {
  team: TeamView;
  /** Every run the window holds, so a live task can be named as the reason a limit matters. */
  runs: TeamRun[];
}

export function Charter({ team, runs }: CharterProps) {
  return (
    <>
      <TeamForm existing={team} runs={runs} />
      <ContextRefs ownerKind="team" ownerId={team.id} />
      <ScopedMemory scopeKind="team" scopeId={team.id} />
      <ScopedTools ownerKind="team" ownerId={team.id} />
    </>
  );
}

/**
 * What a hosting dialog needs to own the submit button: the form's id (the button sits outside the
 * `<form>`, in the Modal footer, and points at it with `form=`) and a report of whether it may be
 * pressed. Passing this also turns the five sections into plain headed groups and drops the save
 * bar, because a card inside a modal card, with a bar that scrolls away, is the wrong dress there.
 */
export interface DialogHost {
  formId: string;
  onState: (state: { canSubmit: boolean; busy: boolean }) => void;
}

/** The console's create form — the same five sections, with nothing to drift from yet. */
export function NewDepartment({ dialog }: { dialog?: DialogHost } = {}) {
  return <TeamForm existing={null} runs={[]} dialog={dialog} />;
}

/** What the guard is holding while it waits for one of the three answers. */
interface Guard {
  drifted: Drift[];
  fresh: TeamView;
}

function TeamForm({
  existing,
  runs,
  dialog,
}: {
  existing: TeamView | null;
  runs: TeamRun[];
  dialog?: DialogHost;
}) {
  const agents = useAgents();
  const teams = useTeams();
  const create = useCreateTeam();
  const update = useUpdateTeam();
  const navigate = useNavigate();

  // The same query the bench already holds, so this shares its cache; `refetch`
  // is what gives the guard a fresh reading at the moment of submit.
  const reread = useTeam(existing?.id ?? "");

  const [form, setForm] = useState<TeamFormState | null>(existing === null ? emptyTeamForm() : null);
  /* Seeded once. A poll tick must not overwrite a half-typed edit — the
     `BudgetPanel` precedent, and the reason the guard below has to exist. */
  const [seed, setSeed] = useState<TeamView | null>(null);
  const [touched, setTouched] = useState<Set<DriftField>>(new Set());
  const [guard, setGuard] = useState<Guard | null>(null);
  const [checking, setChecking] = useState(false);
  const [unreadable, setUnreadable] = useState(false);

  useEffect(() => {
    if (existing !== null && form === null) {
      setForm(teamFormFromView(existing));
      setSeed(existing);
    }
  }, [existing, form]);

  const reportState = dialog?.onState;
  const canSubmit =
    form !== null &&
    form.name.trim() !== "" &&
    form.mission.trim() !== "" &&
    form.directorAgentId.trim() !== "";
  const busy = create.isPending || update.isPending || checking;
  useEffect(() => {
    reportState?.({ canSubmit, busy });
  }, [reportState, canSubmit, busy]);

  if (form === null) return <Quiet says="reading the team…" />;

  const mutation = existing === null ? create : update;
  const valid = form.name.trim() !== "" && form.mission.trim() !== "" && form.directorAgentId.trim() !== "";
  const dirty = existing === null || touched.size > 0;

  /** Every edit marks its field, which is what makes the guard able to stay quiet. */
  function edit(field: DriftField, next: Partial<TeamFormState>) {
    setForm((current) => (current === null ? current : { ...current, ...next }));
    setTouched((current) => new Set(current).add(field));
  }

  function send(body: TeamRequest) {
    if (existing === null) {
      create.mutate(body, {
        onSuccess: (view) => {
          setForm(emptyTeamForm());
          setTouched(new Set());
          void navigate({ to: `/teams/${view.id}` });
        },
      });
      return;
    }
    update.mutate(
      { id: existing.id, body },
      {
        onSuccess: (view) => {
          // Saving makes the daemon's answer the new seed. Without this the
          // next save would compare against a reading two saves old and raise
          // a drift the person themselves caused.
          setSeed(view);
          setForm(teamFormFromView(view));
          setTouched(new Set());
        },
      },
    );
  }

  async function submit() {
    if (form === null || !valid || mutation.isPending || checking) return;
    setUnreadable(false);

    if (existing === null || seed === null) {
      send(teamRequestFromForm(form));
      return;
    }

    setChecking(true);
    const answer = await reread.refetch();
    setChecking(false);

    /*
      `isSuccess`, not `data !== undefined`, and the difference is the whole
      guard. A refetch that FAILS still hands back whatever was in the cache —
      which here is the seed itself — so a check written against `data` would
      compare the seed with the seed, find no drift, and go on to send a full
      replace built on a reading it never actually got. That is precisely the
      silent save this exists to prevent, and it was found by the test below
      rather than by reasoning.
    */
    if (!answer.isSuccess || answer.data === undefined) {
      setUnreadable(true);
      return;
    }

    const drifted = detectDrift(snapshotFromView(seed), snapshotFromView(answer.data), touched);
    if (drifted.length === 0) {
      send(teamRequestFromForm(form));
      return;
    }
    setGuard({ drifted, fresh: answer.data });
  }

  const shared = (teams.data ?? []).filter((other) => other.id !== existing?.id);

  // Every agent that exists, plus any ticked member the catalogue no longer has, so a stale
  // member can still be unticked rather than silently riding along in the request.
  const known = agents.data ?? [];
  const members = [
    ...known.map((agent) => ({ id: agent.id, name: agent.name, speciality: agent.speciality })),
    ...form.members
      .filter((id) => !known.some((agent) => agent.id === id))
      .map((id) => ({ id, name: id, speciality: "no longer in the catalogue" })),
  ];

  return (
    <form
      id={dialog?.formId}
      className={dialog === undefined ? "teams-charter" : "teams-charter teams-charter-dialog"}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <Section
        flat={dialog !== undefined}
        title="Identity"
        note="Renaming a team later is safe: it keeps its link and its history."
      >
        <Field label="Name" required>
          <input
            className="teams-input"
            aria-required="true"
            value={form.name}
            onChange={(event) => edit("name", { name: event.target.value })}
          />
        </Field>
        <Field label="Mission" required>
          <textarea
            className="teams-textarea"
            rows={2}
            aria-required="true"
            value={form.mission}
            onChange={(event) => edit("mission", { mission: event.target.value })}
          />
        </Field>
      </Section>

      <Section
        flat={dialog !== undefined}
        title="Leadership"
        note="The director plans each task and hands out the work. Without one, this team cannot start a task."
      >
        <Field label="Director" required>
          <select
            className="teams-select"
            aria-required="true"
            value={form.directorAgentId}
            onChange={(event) => edit("directorAgentId", { directorAgentId: event.target.value })}
          >
            <option value="">choose an agent</option>
            {(agents.data ?? []).map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.name}
              </option>
            ))}
          </select>
        </Field>
      </Section>

      <Section
        flat={dialog !== undefined}
        title="Staff"
        note="Who this team can give work to. A specialist can serve several teams — where else they work is shown below. Saving replaces the whole roster with what is ticked."
      >
        <fieldset className="teams-members">
          <legend className="teams-label">Members</legend>
          {members.length === 0 ? (
            <p className="teams-note">No specialists exist yet — hire one on the Agents page first.</p>
          ) : (
            <ul className="teams-check-list">
              {members.map((agent) => (
                <li key={agent.id}>
                  <label className="teams-check">
                    <input
                      type="checkbox"
                      checked={form.members.includes(agent.id)}
                      onChange={(event) =>
                        edit("members", {
                          members: event.target.checked
                            ? [...form.members, agent.id]
                            : form.members.filter((id) => id !== agent.id),
                        })
                      }
                    />
                    <span className="teams-check-name">{agent.name}</span>
                    {agent.speciality !== "" && (
                      <span className="teams-check-note">{agent.speciality}</span>
                    )}
                  </label>
                </li>
              ))}
            </ul>
          )}
        </fieldset>
        {form.members.length > 0 && (
          <div className="teams-charter-people">
            {form.members.map((id) => {
              const elsewhere = shared.filter(
                (other) => other.members.includes(id) || other.director_agent_id === id,
              );
              return (
                <Who
                  key={id}
                  id={id}
                  leads={id === form.directorAgentId}
                  shared={elsewhere.length > 0}
                  note={elsewhere.length === 0 ? undefined : `also ${elsewhere.map((o) => o.name).join(", ")}`}
                />
              );
            })}
          </div>
        )}
      </Section>

      <Section
        flat={dialog !== undefined}
        title="Powers"
        note="What this team may do without asking you. Does it: acts on its own. Asks first: drafts it and waits for your OK. Asks you: cannot act, you do it."
      >
        <Grants grants={form.grants} onChange={(grants) => edit("grants", { grants })} />
      </Section>

      <Section
        flat={dialog !== undefined}
        title="Limits"
        note="Some limits cap how much the team does at once; the others cap what each single task may use, counting from zero every time."
      >
        <p className="teams-charter-label">At once</p>
        <div className="teams-charter-limits">
          <Field label="Max live runs (1-4)">
            <input
              className="teams-input"
              type="number"
              min={1}
              max={4}
              value={form.maxLiveRuns}
              onChange={(event) => edit("maxLiveRuns", { maxLiveRuns: event.target.value })}
            />
          </Field>
          {/* Live runs occupy this ceiling right now: the reading Acting Green is for. */}
          <Meter
            label="at work"
            value={runs.filter((run) => run.team_id === existing?.id && LIVE.has(run.state)).length}
            ceiling={parseCeiling(form.maxLiveRuns)}
            tone="active"
          />
          <Field label="Max open actions (0-20)">
            <input
              className="teams-input"
              type="number"
              min={0}
              max={20}
              value={form.maxOpenActions}
              onChange={(event) => edit("maxOpenActions", { maxOpenActions: event.target.value })}
            />
          </Field>
        </div>

        <p className="teams-charter-label">Per task</p>
        <div className="teams-charter-limits">
          <Field label="Max rounds (1-6)">
            <input
              className="teams-input"
              type="number"
              min={1}
              max={6}
              value={form.maxRounds}
              onChange={(event) => edit("maxRounds", { maxRounds: event.target.value })}
            />
          </Field>
          <Field label="Max parallel (1-8)">
            <input
              className="teams-input"
              type="number"
              min={1}
              max={8}
              value={form.maxParallel}
              onChange={(event) => edit("maxParallel", { maxParallel: event.target.value })}
            />
          </Field>
          <Field label="Budget ceiling (USD, blank = no ceiling)">
            <input
              className="teams-input"
              type="text"
              inputMode="decimal"
              placeholder="no ceiling"
              value={form.budgetUsd}
              onChange={(event) => edit("budgetUsd", { budgetUsd: event.target.value })}
            />
          </Field>
        </div>
        <div className="teams-charter-limits">
          <LimitChip name="rounds" ceiling={parseCeiling(form.maxRounds)} />
          <LimitChip name="parallel" ceiling={parseCeiling(form.maxParallel)} />
          <LimitChip name="spend" ceiling={parseCeiling(form.budgetUsd)} format={usd} />
        </div>
      </Section>

      {guard !== null && (
        <DriftGuard
          guard={guard}
          onTake={() => {
            const merged = takeTheirs(form, guard.fresh, guard.drifted);
            setForm(merged);
            setSeed(guard.fresh);
            setGuard(null);
            send(teamRequestFromForm(merged));
          }}
          onReload={() => {
            setForm(teamFormFromView(guard.fresh));
            setSeed(guard.fresh);
            setTouched(new Set());
            setGuard(null);
          }}
          onAnyway={() => {
            setSeed(guard.fresh);
            setGuard(null);
            send(teamRequestFromForm(form));
          }}
        />
      )}

      {unreadable && (
        <ErrorNote>
          the núcleo did not answer when this form asked what the team looks like now — nothing
          was saved, so nothing of theirs could be overwritten by mistake
        </ErrorNote>
      )}

      {/* Visible only when there is something to save: a bar that is always
          there stops being a signal that anything changed. */}
      {dirty && dialog === undefined && (
        <div className="teams-savebar">
          <p className="teams-savebar-said">
            {existing === null
              ? "Creating sends the roster and the powers as they are here."
              : `${touched.size} ${touched.size === 1 ? "section" : "sections"} changed — the roster and the powers are sent whole, so what is here replaces what is there.`}
          </p>
          <Button type="submit" intent="go" disabled={!valid || mutation.isPending || checking}>
            {checking ? "Checking…" : existing === null ? "Create team" : "Save"}
          </Button>
        </div>
      )}

      {mutation.isError && <SaveRefusal error={mutation.error} />}
    </form>
  );
}

/** `core/src/team.rs:38` — the three states a task is alive in. */
const LIVE = new Set(["planning", "working", "delivering"]);

/**
 * Somebody else changed this while the form was open.
 *
 * Three answers, and the absence of a fourth is the point: there is no way from
 * here to a save that says nothing. `role="alertdialog"` semantics without a
 * modal — the form stays readable behind the question, because deciding needs
 * to see what is in it.
 */
function DriftGuard({
  guard,
  onTake,
  onReload,
  onAnyway,
}: {
  guard: Guard;
  onTake: () => void;
  onReload: () => void;
  onAnyway: () => void;
}) {
  return (
    <div className="teams-drift" role="alert" aria-label="Changed while you were editing">
      <p className="teams-drift-said">
        This team changed while you had the form open, in{" "}
        {guard.drifted.length === 1 ? "a field" : "fields"} you were not editing. Saving as-is would
        replace {guard.drifted.length === 1 ? "it" : "them"} — the roster and the powers go whole.
      </p>
      <ul className="teams-drift-list">
        {guard.drifted.map((one) => (
          <li className="teams-drift-row" key={one.field}>
            <span className="teams-drift-field">{one.label}</span>
            <span className="teams-drift-was">you have: {one.was}</span>
            <span className="teams-drift-now">it is now: {one.now}</span>
          </li>
        ))}
      </ul>
      <div className="teams-act-controls">
        <Button variant="approve" onClick={onTake}>
          Take theirs and save
        </Button>
        <Button onClick={onReload}>Reload the form</Button>
        <Button variant="danger" onClick={onAnyway}>
          Save mine anyway
        </Button>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------- pieces -- */

function Section({
  title,
  note,
  flat = false,
  children,
}: {
  title: string;
  note: string;
  /** In a dialog: a plain headed group instead of a `Panel`. */
  flat?: boolean;
  children: React.ReactNode;
}) {
  if (flat) {
    return (
      <section className="teams-group">
        <h3 className="teams-group-title">{title}</h3>
        <p className="teams-note">{note}</p>
        <div className="teams-section-fields">{children}</div>
      </section>
    );
  }
  return (
    <Panel title={title} variant="flat">
      <div className="teams-section">
        <p className="teams-note">{note}</p>
        <div className="teams-section-fields">{children}</div>
      </div>
    </Panel>
  );
}

function Field({
  label,
  required = false,
  children,
}: {
  label: string;
  required?: boolean;
  children: React.ReactNode;
}) {
  return (
    <label className="teams-field">
      <span className="teams-label" data-required={required ? "true" : undefined}>
        {label}
      </span>
      {children}
    </label>
  );
}

/**
 * The human name of each grantable kind, shared with the console so the table and the dialog say
 * the same thing. The identifier the daemon uses stays visible, muted, beside it in the dialog.
 */
export const POWER_LABEL: Record<(typeof GRANTABLE_ACTIONS)[number], string> = {
  calendar_event: "Calendar",
  file_document: "Documents",
  send_email: "Email",
};

/**
 * One row per grantable action, three states: nothing / asks first / does it.
 *
 * The absence of a grant row IS the denial — there is no `deny` mode
 * (`core/src/team.rs:375`). `propose` reads "asks first", `allow` reads "does
 * it", and nothing at all is "asks you", the same three words the console's
 * table uses, which is why the empty option is spelled out rather than being a
 * blank line at the top of the list.
 */
function Grants({ grants, onChange }: { grants: TeamGrant[]; onChange: (grants: TeamGrant[]) => void }) {
  return (
    <ul className="teams-grants" aria-label="Grants">
      {GRANTABLE_ACTIONS.map((kind) => {
        const current = grants.find((grant) => grant.kind === kind)?.mode ?? "";
        return (
          <li className="teams-grant" key={kind}>
            <span className="teams-grant-label">
              {POWER_LABEL[kind]}
              <span className="teams-grant-ident">{kind}</span>
            </span>
            <select
              className="teams-select teams-grant-modes"
              aria-label={`${POWER_LABEL[kind]} grant`}
              value={current}
              onChange={(event) => {
                const value = event.target.value;
                const rest = grants.filter((grant) => grant.kind !== kind);
                onChange(value === "" ? rest : [...rest, { kind, mode: value }]);
              }}
            >
              <option value="">asks you</option>
              {GRANT_MODES.map((mode) => (
                <option key={mode} value={mode}>
                  {mode === "propose" ? "asks first" : "does it"}
                </option>
              ))}
            </select>
          </li>
        );
      })}
    </ul>
  );
}

function SaveRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this team was not saved</ErrorNote>;
}
