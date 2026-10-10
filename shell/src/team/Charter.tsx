// §spec alcada-por-equipa
import { useEffect, useId, useRef, useState } from "react";
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
import { Button, ErrorNote, Field, Meter, Panel, Quiet, RefusalNote } from "../ui";
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
 * separates: what the whole team does at once, and what each single task may
 * use. The one ceiling something occupies right now carries its live reading.
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

/**
 * The numeric ceilings a person can get wrong, with the range each accepts.
 *
 * `Number("")` is `0`, which is a legal-looking ceiling nobody typed, so the range is checked on
 * the raw string and a blank is a problem for every field except the budget, where blank means
 * "no ceiling".
 */
interface LimitSpec {
  field: "maxLiveRuns" | "maxOpenActions" | "maxRounds" | "maxParallel" | "budgetUsd";
  min: number;
  max: number | null;
  integer: boolean;
  blankOk: boolean;
}

const LIMITS: Record<LimitSpec["field"], LimitSpec> = {
  maxLiveRuns: { field: "maxLiveRuns", min: 1, max: 4, integer: true, blankOk: false },
  maxOpenActions: { field: "maxOpenActions", min: 0, max: 20, integer: true, blankOk: false },
  maxRounds: { field: "maxRounds", min: 1, max: 6, integer: true, blankOk: false },
  maxParallel: { field: "maxParallel", min: 1, max: 8, integer: true, blankOk: false },
  budgetUsd: { field: "budgetUsd", min: 0, max: null, integer: false, blankOk: true },
};

/** What to tell the person when a ceiling is out of range, or `null` when it is fine. */
export function limitProblem(field: LimitSpec["field"], raw: string): string | null {
  const spec = LIMITS[field];
  const trimmed = raw.trim();
  if (trimmed === "") return spec.blankOk ? null : rangeSaid(spec);
  const value = Number(trimmed);
  const inRange = Number.isFinite(value) && value >= spec.min && (spec.max === null || value <= spec.max);
  if (!inRange || (spec.integer && !Number.isInteger(value))) return rangeSaid(spec);
  return null;
}

function rangeSaid(spec: LimitSpec): string {
  return spec.max === null ? `Use ${spec.min} or more` : `Use ${spec.min} to ${spec.max}`;
}

/** The limit fields whose current text is not acceptable. */
function limitProblems(form: TeamFormState): LimitSpec["field"][] {
  return (Object.keys(LIMITS) as LimitSpec["field"][]).filter((field) => limitProblem(field, form[field]) !== null);
}

/** The three required words, named as the sentence under a disabled Save needs them. */
function missingPieces(form: TeamFormState): string[] {
  const missing: string[] = [];
  if (form.name.trim() === "") missing.push("a name");
  if (form.mission.trim() === "") missing.push("a mission");
  if (form.directorAgentId.trim() === "") missing.push("a director");
  return missing;
}

/** Which section of the form owns each field, so the save bar can count sections and not fields. */
const SECTION_OF: Record<DriftField, string> = {
  name: "Identity",
  mission: "Identity",
  directorAgentId: "Leadership",
  members: "Staff",
  grants: "Powers",
  maxRounds: "Limits",
  maxParallel: "Limits",
  budgetUsd: "Limits",
  maxOpenActions: "Limits",
  maxLiveRuns: "Limits",
};

function joinWords(words: string[]): string {
  if (words.length <= 1) return words.join("");
  return `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
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
  // "Saved" is shown for a moment where the save bar was, then goes — see the effect below.
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (existing !== null && form === null) {
      setForm(teamFormFromView(existing));
      setSeed(existing);
    }
  }, [existing, form]);

  const reportState = dialog?.onState;
  const canSubmit = form !== null && missingPieces(form).length === 0 && limitProblems(form).length === 0;
  const busy = create.isPending || update.isPending || checking;
  useEffect(() => {
    reportState?.({ canSubmit, busy });
  }, [reportState, canSubmit, busy]);

  useEffect(() => {
    if (!saved) return;
    const timer = setTimeout(() => setSaved(false), 2500);
    return () => clearTimeout(timer);
  }, [saved]);

  if (form === null) return <Quiet says="reading the team…" />;

  const mutation = existing === null ? create : update;
  const missing = missingPieces(form);
  const badLimits = limitProblems(form);
  const valid = missing.length === 0 && badLimits.length === 0;
  const dirty = existing === null || touched.size > 0;
  const sectionsChanged = new Set([...touched].map((field) => SECTION_OF[field])).size;
  const whyNot: "create" | "save" = existing === null ? "create" : "save";

  /** Every edit marks its field, which is what makes the guard able to stay quiet. */
  function edit(field: DriftField, next: Partial<TeamFormState>) {
    setForm((current) => (current === null ? current : { ...current, ...next }));
    setTouched((current) => new Set(current).add(field));
    setSaved(false);
  }

  /*
    A required box says nothing until somebody empties it. A standing "Required" under every
    filled field was noise the eye learned to skip, so the words now appear only when they are
    true, and only once the person has touched the field — a fresh create form is not an error.
  */
  function emptied(field: DriftField, value: string, said: string): React.ReactNode {
    if (!touched.has(field) || value.trim() !== "") return undefined;
    return <span className="teams-limit-error">{said}</span>;
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
          setSaved(true);
        },
      },
    );
  }

  async function submit() {
    if (form === null || !valid || !dirty || mutation.isPending || checking) return;
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
      onKeyDown={(event) => {
        // Ctrl/Cmd+S saves by the same road as the button, drift check included.
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
          event.preventDefault();
          void submit();
        }
      }}
    >
      <Section
        flat={dialog !== undefined}
        title="Identity"
        note="Renaming a team later is safe: it keeps its link and its history."
      >
        <Field label="Name" helper={emptied("name", form.name, "Add a name")}>
          <input
            className="teams-input"
            aria-required="true"
            value={form.name}
            onChange={(event) => edit("name", { name: event.target.value })}
          />
        </Field>
        <Field label="Mission" helper={emptied("mission", form.mission, "Add a mission")}>
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
        <Field label="Director" helper={emptied("directorAgentId", form.directorAgentId, "Choose a director")}>
          <select
            className="teams-select"
            aria-required="true"
            value={form.directorAgentId}
            onChange={(event) => edit("directorAgentId", { directorAgentId: event.target.value })}
          >
            <option value="">Choose an agent</option>
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
        note="Who this team can give work to. A specialist can serve several teams; where else they work is shown beside their name. Saving replaces the whole roster with what is ticked."
      >
        <fieldset className="teams-members">
          <legend className="teams-label">Members</legend>
          {members.length === 0 ? (
            <p className="teams-note">No specialists exist yet — hire one on the Agents page first.</p>
          ) : (
            <ul className="teams-check-list">
              {members.map((agent) => {
                // Said in the row it is about. A second row of chips under the list repeated
                // every ticked name only to hang this one fact on it.
                const elsewhere = shared.filter(
                  (other) => other.members.includes(agent.id) || other.director_agent_id === agent.id,
                );
                return (
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
                      {/* One text block beside the box: name and speciality run on as a
                          sentence, and "also in" takes its own line under them rather than a
                          right-hand column that broke into ragged fragments. */}
                      <span className="teams-check-body">
                        <span className="teams-check-name">{agent.name}</span>
                        {agent.speciality !== "" && (
                          <span className="teams-check-note">{agent.speciality}</span>
                        )}
                        {elsewhere.length > 0 && (
                          <span className="teams-check-also">
                            also in {elsewhere.map((other) => other.name).join(", ")}
                          </span>
                        )}
                      </span>
                    </label>
                  </li>
                );
              })}
            </ul>
          )}
        </fieldset>
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
        note="Two of these cap how much the team does at once. The rest cap what each single task may use, counting from zero every time."
      >
        <LimitGroup title="The whole team, at once">
          <StepperRow
            field="maxLiveRuns"
            label="Tasks running at the same time"
            explain="How many tasks this team may work on at once."
            fewer="Fewer tasks at once"
            more="More tasks at once"
            value={form.maxLiveRuns}
            onChange={(value) => edit("maxLiveRuns", { maxLiveRuns: value })}
          >
            {existing !== null && (
              <LiveNow
                value={runs.filter((run) => run.team_id === existing.id && LIVE.has(run.state)).length}
                ceiling={parseCeiling(form.maxLiveRuns)}
              />
            )}
          </StepperRow>
          <StepperRow
            field="maxOpenActions"
            label="Actions waiting for your OK"
            explain="How many drafted actions may wait for your decision at once."
            fewer="Fewer actions waiting"
            more="More actions waiting"
            value={form.maxOpenActions}
            onChange={(value) => edit("maxOpenActions", { maxOpenActions: value })}
          />
        </LimitGroup>

        <LimitGroup title="Each task">
          <StepperRow
            field="maxRounds"
            label="Rounds per task"
            explain="How many times the director may plan, hand out work and review before the task ends."
            fewer="Fewer rounds"
            more="More rounds"
            value={form.maxRounds}
            onChange={(value) => edit("maxRounds", { maxRounds: value })}
          />
          <StepperRow
            field="maxParallel"
            label="Specialists working in parallel"
            explain="How many specialists may work on one task at the same time."
            fewer="Fewer specialists in parallel"
            more="More specialists in parallel"
            value={form.maxParallel}
            onChange={(value) => edit("maxParallel", { maxParallel: value })}
          />
          <CeilingRow value={form.budgetUsd} onChange={(value) => edit("budgetUsd", { budgetUsd: value })} />
        </LimitGroup>
      </Section>

      {guard !== null && (
        <DriftGuard
          guard={guard}
          onKeep={() => setGuard(null)}
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
          NucleOS didn&apos;t answer when the form asked what the team looks like now, so nothing was
          saved. Try {whyNot === "create" ? "again" : "Save again"}.
        </ErrorNote>
      )}

      {/* Visible only when there is something to save: a bar that is always
          there stops being a signal that anything changed. */}
      {dirty && dialog === undefined && (
        <div className="teams-savebar">
          <p className="teams-savebar-said">
            {!valid
              ? whyCannot(missing, badLimits.length > 0, whyNot)
              : existing === null
                ? "Creating sends the roster and the powers as they are here."
                : `${sectionsChanged} ${sectionsChanged === 1 ? "section" : "sections"} changed — the roster and the powers are sent whole, so what is here replaces what is there.`}
          </p>
          <Button type="submit" intent="go" disabled={!valid || mutation.isPending || checking}>
            {checking
              ? "Checking…"
              : mutation.isPending
                ? existing === null
                  ? "Creating…"
                  : "Saving…"
                : existing === null
                  ? "Create team"
                  : "Save"}
          </Button>
        </div>
      )}

      {!dirty && saved && dialog === undefined && (
        <p className="teams-savebar-saved" role="status">
          Saved
        </p>
      )}

      {mutation.isError && <SaveRefusal error={mutation.error} creating={existing === null} />}
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
  onKeep,
  onAnyway,
}: {
  guard: Guard;
  onTake: () => void;
  onReload: () => void;
  onKeep: () => void;
  onAnyway: () => void;
}) {
  // The question appears below the sections, which can be off-screen on a tall form: bring it
  // into view and give it focus, so neither a sighted nor a keyboard user is left looking at a
  // Save that did nothing.
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = ref.current;
    if (node === null) return;
    node.scrollIntoView?.({ block: "nearest" });
    node.focus();
  }, []);

  return (
    <div
      ref={ref}
      tabIndex={-1}
      className="teams-drift"
      role="alert"
      aria-label="Changed while you were editing"
    >
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
        <Button variant="danger" onClick={onReload}>
          Discard my edits and reload
        </Button>
        <Button onClick={onKeep}>Keep editing</Button>
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

/**
 * One group of ceilings, drawn as the Powers rows are: a bordered row per setting, its name on the
 * left and its control on the right. The limits used to be a grid of bare boxes under uppercase
 * eyebrows, and read as a spreadsheet nobody had explained; a list of sentences with a control
 * each reads as the settings they are.
 */
function LimitGroup({ title, children }: { title: string; children: React.ReactNode }) {
  const titleId = useId();
  return (
    <div className="teams-limit-group" role="group" aria-labelledby={titleId}>
      <p className="teams-limit-group-title" id={titleId}>
        {title}
      </p>
      <ul className="teams-grants teams-limits">{children}</ul>
    </div>
  );
}

/** The range a count accepts, written the way the explanation line says it: "1–4". */
function rangeShort(spec: LimitSpec): string {
  return spec.max === null ? `${spec.min} or more` : `${spec.min}–${spec.max}`;
}

/**
 * A count with a stepper. The number stays an input (somebody who knows they want 6 types 6),
 * but the buttons and the arrow keys only ever land inside the range, so the common change is a
 * click that cannot be wrong. A typed value outside it is still possible, and still answered by
 * `limitProblem`, which is what keeps Save honest.
 */
function StepperRow({
  field,
  label,
  explain,
  fewer,
  more,
  value,
  onChange,
  children,
}: {
  field: Exclude<LimitSpec["field"], "budgetUsd">;
  label: string;
  explain: string;
  /** The buttons' accessible names: "−" and "+" alone say nothing about what they change. */
  fewer: string;
  more: string;
  value: string;
  onChange: (value: string) => void;
  /** A reading that belongs to this ceiling, drawn under its explanation. */
  children?: React.ReactNode;
}) {
  const inputId = useId();
  const explainId = useId();
  const problemId = useId();
  const spec = LIMITS[field];
  const max = spec.max ?? Number.POSITIVE_INFINITY;
  const problem = limitProblem(field, value);
  const typed = Number(value.trim());
  const readable = value.trim() !== "" && Number.isFinite(typed);

  function step(by: number) {
    // From an unreadable box the first step lands on the nearest bound, never on NaN.
    const from = readable ? Math.round(typed) : by > 0 ? spec.min - 1 : max + 1;
    onChange(String(Math.min(max, Math.max(spec.min, from + by))));
  }

  return (
    <li className="teams-grant teams-limit">
      <div className="teams-limit-text">
        <label className="teams-limit-label" htmlFor={inputId}>
          {label}
        </label>
        <span className="teams-limit-explain" id={explainId}>
          {explain} {rangeShort(spec)}.
        </span>
        {problem !== null && (
          <span className="teams-limit-error" id={problemId}>
            {problem}
          </span>
        )}
        {children}
      </div>
      <div className="teams-stepper">
        <button
          type="button"
          className="teams-stepper-button"
          aria-label={fewer}
          disabled={readable && typed <= spec.min}
          onClick={() => step(-1)}
        >
          −
        </button>
        <input
          id={inputId}
          className="teams-stepper-input"
          type="text"
          inputMode="numeric"
          aria-describedby={problem === null ? explainId : `${explainId} ${problemId}`}
          aria-invalid={problem === null ? undefined : true}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "ArrowUp" || event.key === "ArrowDown") {
              event.preventDefault();
              step(event.key === "ArrowUp" ? 1 : -1);
            }
          }}
        />
        <button
          type="button"
          className="teams-stepper-button"
          aria-label={more}
          disabled={readable && typed >= max}
          onClick={() => step(1)}
        >
          +
        </button>
      </div>
    </li>
  );
}

/**
 * The live reading under "Tasks running at the same time": the ceiling a person is setting, with
 * what is occupying it right now. It sat in the limits grid as a cell of its own, between two
 * inputs, where it read as a third setting.
 */
function LiveNow({ value, ceiling }: { value: number; ceiling: number | null }) {
  return (
    <span className="teams-limit-live">
      <span className="teams-limit-live-said">{value} running now</span>
      <Meter label="running now" value={value} ceiling={ceiling} tone="active" head={false} />
    </span>
  );
}

/**
 * The spending ceiling: an amount, or none at all. "None" is a box to tick rather than a blank
 * to leave, because a blank box reads as something not yet filled in. What is saved is the same
 * as before: no ceiling is a blank, sent as `null` and never as `0`, which is a real ceiling.
 */
function CeilingRow({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const inputId = useId();
  const explainId = useId();
  const problemId = useId();
  const amount = useRef<HTMLInputElement>(null);
  // The last amount typed, so ticking "No ceiling" and unticking it again gives it back.
  const remembered = useRef(value);
  const [none, setNone] = useState(value.trim() === "");
  const problem = limitProblem("budgetUsd", value);

  useEffect(() => {
    if (value.trim() !== "") {
      remembered.current = value;
      setNone(false);
    }
  }, [value]);

  return (
    <li className="teams-grant teams-limit">
      <div className="teams-limit-text">
        <label className="teams-limit-label" htmlFor={inputId}>
          Spending ceiling per task
        </label>
        <span className="teams-limit-explain" id={explainId}>
          What one task may spend, in US dollars, before it stops.
        </span>
        {problem !== null && (
          <span className="teams-limit-error" id={problemId}>
            {problem}
          </span>
        )}
      </div>
      <div className="teams-ceiling">
        <span className="teams-money" data-off={none ? "true" : undefined}>
          <span className="teams-money-sign" aria-hidden="true">
            $
          </span>
          <input
            id={inputId}
            ref={amount}
            className="teams-money-input"
            type="text"
            inputMode="decimal"
            placeholder={none ? "" : "0.00"}
            disabled={none}
            aria-describedby={problem === null ? explainId : `${explainId} ${problemId}`}
            aria-invalid={problem === null ? undefined : true}
            value={value}
            onChange={(event) => onChange(event.target.value)}
            onBlur={() => {
              // Emptied and left: that is no ceiling, so the box says so instead of a blank
              // that quietly means the same thing.
              if (value.trim() === "") setNone(true);
            }}
          />
        </span>
        <label className="teams-check teams-ceiling-none">
          <input
            type="checkbox"
            checked={none}
            onChange={(event) => {
              const ticked = event.target.checked;
              setNone(ticked);
              if (ticked) {
                onChange("");
              } else {
                onChange(remembered.current);
                // Focus waits for the box to be enabled, which is the next render.
                setTimeout(() => amount.current?.focus(), 0);
              }
            }}
          />
          <span className="teams-check-name">No ceiling</span>
        </label>
      </div>
    </li>
  );
}

/** The sentence under a disabled Save: what is actually missing, not a generic refusal. */
function whyCannot(missing: string[], limitsOff: boolean, verb: "save" | "create"): string {
  if (missing.length > 0) return `Add ${joinWords(missing)} to ${verb}`;
  return limitsOff ? `Fix the limits marked above to ${verb}` : `Fill in the form to ${verb}`;
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
            <span className="teams-grant-label" title={kind}>
              {POWER_LABEL[kind]}
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
              <option value="">Asks you</option>
              {GRANT_MODES.map((mode) => (
                <option key={mode} value={mode}>
                  {mode === "propose" ? "Asks first" : "Does it"}
                </option>
              ))}
            </select>
          </li>
        );
      })}
    </ul>
  );
}

function SaveRefusal({ error, creating }: { error: unknown; creating: boolean }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return (
    <ErrorNote>
      NucleOS didn&apos;t answer, so nothing was saved. Try {creating ? "again" : "Save again"}.
    </ErrorNote>
  );
}
