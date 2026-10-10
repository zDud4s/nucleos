import { useEffect, useRef, useState, type RefObject } from "react";
import { isApiRefusal } from "../data/client";
import {
  TRIGGER_SOURCES,
  useCreateTrigger,
  useDeleteTrigger,
  useSetTriggerEnabled,
  useTeams,
  useTriggerNext,
  type TeamTrigger,
  type TeamView,
  type TriggerRequest,
} from "../data/teams";
import {
  Button,
  ConfirmButton,
  ErrorNote,
  Inset,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  StateBadge,
} from "../ui";
import { daemonProse } from "./prose";

/**
 * `Routines` — the rules that start this department without anybody asking.
 *
 * **There is no Edit, and there cannot be.** The núcleo mounts `POST`,
 * `DELETE` and `/enable` on `/team-triggers` and nothing else — no `PUT`, no
 * `PATCH` — so an Edit button would answer 405. What this offers instead is
 * **Duplicate**: it fills the write-a-rule form with the rule's own values, so
 * changing one is write-the-new-one-then-delete-the-old-one, done in the order
 * that never leaves the department with no rule at all.
 *
 * The one interlock on this tab is **arming a rule for a department with no
 * budget ceiling**. That is the single control in this whole pillar that can
 * spend without bound — a clock that starts a task that has no limit — and it
 * is therefore the only one that goes through `ConfirmButton`. Everything else
 * stays plain, per the standing rule that the interlock is for destructive
 * writes, plus this one.
 */

export interface RoutinesProps {
  team: TeamView;
  /** Already filtered to this department by the bench. */
  rules: TeamTrigger[];
}

/** What a source id reads as to a person; the ids themselves are the daemon's. */
const SOURCE_LABELS: Record<string, string> = {
  cron: "On a schedule",
  team_finished: "After another team finishes",
  email_triaged: "When an email is triaged",
};

function sourceLabel(source: string): string {
  return SOURCE_LABELS[source] ?? source;
}

/** Every IANA zone the runtime knows, for the Timezone suggestions; empty where unsupported. */
function timeZones(): string[] {
  const intl = Intl as unknown as { supportedValuesOf?: (key: string) => string[] };
  try {
    return intl.supportedValuesOf?.("timeZone") ?? [];
  } catch {
    return [];
  }
}

/** The write-a-rule form's fields, which `Duplicate` fills wholesale. */
interface RuleDraft {
  name: string;
  source: (typeof TRIGGER_SOURCES)[number];
  cron: string;
  timezone: string;
  fromTeam: string;
  emailClass: string;
  request: string;
}

function emptyDraft(): RuleDraft {
  return { name: "", source: "cron", cron: "", timezone: "", fromTeam: "", emailClass: "", request: "" };
}

/** A rule, as a draft of another one. The name is marked so two are never confused. */
function draftFrom(rule: TeamTrigger): RuleDraft {
  return {
    name: `${rule.name} (copy)`,
    source: (TRIGGER_SOURCES as readonly string[]).includes(rule.source)
      ? (rule.source as (typeof TRIGGER_SOURCES)[number])
      : "cron",
    cron: rule.cron ?? "",
    timezone: rule.timezone ?? "",
    fromTeam: rule.from_team ?? "",
    emailClass: rule.email_class ?? "",
    request: rule.request,
  };
}

export function Routines({ team, rules }: RoutinesProps) {
  const [draft, setDraft] = useState<RuleDraft>(emptyDraft);
  const nameRef = useRef<HTMLInputElement>(null);
  // Bumped by every Duplicate, so a second click on the same rule still answers.
  const [duplicated, setDuplicated] = useState(0);

  // Duplicate fills a form that may be off-screen; bring the person to it once the draft is in.
  useEffect(() => {
    if (duplicated === 0) return;
    nameRef.current?.focus();
    nameRef.current?.scrollIntoView?.({ block: "center" });
  }, [duplicated]);

  return (
    <div className="teams-routines">
      <Panel title="Rules">
        <p className="teams-note">
          Rules can't be edited. Duplicate one to make a changed copy, then delete the original.
        </p>
        {rules.length === 0 && <Quiet says="No rules yet. Write one below — it stays off until you arm it." />}
        {rules.length > 0 && (
          <ul className="teams-rules" aria-label="Rules">
            {rules.map((rule) => (
              <RuleRow
                key={rule.id}
                rule={rule}
                noCeiling={team.budget_usd === null}
                onDuplicate={() => {
                  setDraft(draftFrom(rule));
                  setDuplicated((n) => n + 1);
                }}
              />
            ))}
          </ul>
        )}
      </Panel>

      <Panel title="Write a rule">
        <NewRuleForm
          teamId={team.id}
          draft={draft}
          nameRef={nameRef}
          onChange={setDraft}
          onWritten={() => setDraft(emptyDraft())}
        />
      </Panel>
    </div>
  );
}

function RuleRow({
  rule,
  noCeiling,
  onDuplicate,
}: {
  rule: TeamTrigger;
  noCeiling: boolean;
  onDuplicate: () => void;
}) {
  const setEnabled = useSetTriggerEnabled();
  const del = useDeleteTrigger();
  // `enabled` arrives 0 or 1 on the wire, never a real boolean.
  const armed = rule.enabled !== 0;

  return (
    <Inset as="li">
      <div className="teams-rule-head">
        <span className="teams-rule-name">{rule.name}</span>
        <StateBadge domain="setting" state={armed ? "armed" : "disarmed"} />
        {/* One phrase — "On a schedule · 0 9 * * 1-5 · Europe/Lisbon" — with the cron alone in mono. */}
        <span className="teams-rule-source">
          {sourceLabel(rule.source)}
          {rule.cron !== null && (
            <>
              {" · "}
              <code className="teams-rule-cron">{rule.cron}</code>
            </>
          )}
          {rule.timezone !== null && ` · ${rule.timezone}`}
          {rule.from_team !== null && ` · ${rule.from_team}`}
          {rule.email_class !== null && ` · ${rule.email_class}`}
        </span>
      </div>

      <p className="teams-rule-what">{rule.request}</p>
      <NextLine id={rule.id} />

      <div className="teams-rule-controls">
        {armed ? (
          // Disarming is always plain and never asks.
          <Button onClick={() => setEnabled.mutate({ id: rule.id, enabled: false })} disabled={setEnabled.isPending}>
            {setEnabled.isPending ? "Disarming…" : "Disarm"}
          </Button>
        ) : noCeiling ? (
          // An armed rule on a department with no ceiling is a clock that can
          // spend without limit — the one place in this design where being
          // wrong costs money without bound.
          <ConfirmButton
            label={setEnabled.isPending ? "Arming…" : "Arm with no ceiling"}
            confirmLabel="Arm it anyway"
            variant="approve"
            intent="go"
            disabled={setEnabled.isPending}
            onConfirm={() => setEnabled.mutate({ id: rule.id, enabled: true })}
          />
        ) : (
          <Button
            intent="go"
            onClick={() => setEnabled.mutate({ id: rule.id, enabled: true })}
            disabled={setEnabled.isPending}
          >
            {setEnabled.isPending ? "Arming…" : "Arm"}
          </Button>
        )}
        {/* Duplicate, never Edit: `DELETE` is the only other verb this route has. */}
        <Button onClick={onDuplicate}>Duplicate</Button>
        <ConfirmButton
          label={del.isPending ? "Deleting…" : "Delete"}
          confirmLabel="Delete this rule"
          variant="danger"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(rule.id)}
        />
      </div>
      {(setEnabled.isError || del.isError) && (
        <CreateRefusal what="change this rule" error={setEnabled.error ?? del.error} />
      )}
    </Inset>
  );
}

/**
 * When one rule fires next, with its own `useTriggerNext(id)` — the row-scoped
 * hook pattern.
 *
 * A rule that does not fire on a clock answers 200 with
 * `"this rule does not fire on a clock"`, and a bad cron answers 200 with the
 * daemon's parse message. Neither is an error state and neither is drawn as one.
 */
function NextLine({ id }: { id: number }) {
  const next = useTriggerNext(id);
  if (next.data === undefined) return null;
  if (next.data.error !== null) return <p className="teams-rule-next">{next.data.error}</p>;
  if (next.data.next !== null) {
    return (
      <p className="teams-rule-next">
        next: <RelativeTime at={next.data.next} />
      </p>
    );
  }
  return null;
}

/**
 * Write a rule — and the same form `Duplicate` fills.
 *
 * Controlled from above rather than holding its own state, which is what makes
 * Duplicate possible at all: the parent owns the draft, so a click on a row can
 * put a rule's values into it.
 */
function NewRuleForm({
  teamId,
  draft,
  nameRef,
  onChange,
  onWritten,
}: {
  teamId: string;
  draft: RuleDraft;
  nameRef: RefObject<HTMLInputElement | null>;
  onChange: (draft: RuleDraft) => void;
  onWritten: () => void;
}) {
  const create = useCreateTrigger();
  const teams = useTeams();
  const zones = timeZones();
  // Every other known team; a duplicated rule's own source team stays selectable even if it is gone.
  const otherTeams = (Array.isArray(teams.data) ? teams.data : []).filter((t) => t.id !== teamId);
  const fromKnown = otherTeams.some((t) => t.id === draft.fromTeam);
  const valid =
    draft.name.trim() !== "" && draft.request.trim() !== "" && (draft.source !== "cron" || draft.cron.trim() !== "");

  function submit() {
    if (!valid || create.isPending) return;
    const body: TriggerRequest = {
      team_id: teamId,
      name: draft.name.trim(),
      source: draft.source,
      cron: draft.source === "cron" ? draft.cron.trim() : null,
      timezone: draft.source === "cron" && draft.timezone.trim() !== "" ? draft.timezone.trim() : null,
      from_team: draft.source === "team_finished" && draft.fromTeam.trim() !== "" ? draft.fromTeam.trim() : null,
      email_class:
        draft.source === "email_triaged" && draft.emailClass.trim() !== "" ? draft.emailClass.trim() : null,
      request: draft.request.trim(),
    };
    create.mutate(body, { onSuccess: onWritten });
  }

  return (
    <form
      className="teams-form"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <p className="teams-note">Writing never arms — arm the rule once it is written.</p>
      <label className="teams-field">
        <span className="teams-label" data-required="true">
          Name
        </span>
        <input
          ref={nameRef}
          className="teams-input"
          aria-required="true"
          value={draft.name}
          onChange={(event) => onChange({ ...draft, name: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Source</span>
        <select
          className="teams-select"
          value={draft.source}
          onChange={(event) =>
            onChange({ ...draft, source: event.target.value as (typeof TRIGGER_SOURCES)[number] })
          }
        >
          {TRIGGER_SOURCES.map((kind) => (
            <option key={kind} value={kind}>
              {sourceLabel(kind)}
            </option>
          ))}
        </select>
      </label>
      {draft.source === "cron" && (
        <>
          <label className="teams-field">
            <span className="teams-label" data-required="true">
              Cron
            </span>
            <input
              className="teams-input teams-input-num"
              aria-required="true"
              placeholder="0 9 * * 1-5"
              value={draft.cron}
              onChange={(event) => onChange({ ...draft, cron: event.target.value })}
            />
          </label>
          <label className="teams-field">
            <span className="teams-label">Timezone</span>
            <input
              className="teams-input teams-input-num"
              list="teams-tz"
              placeholder="Europe/Lisbon"
              value={draft.timezone}
              onChange={(event) => onChange({ ...draft, timezone: event.target.value })}
            />
            {zones.length > 0 && (
              <datalist id="teams-tz">
                {zones.map((zone) => (
                  <option key={zone} value={zone} />
                ))}
              </datalist>
            )}
          </label>
        </>
      )}
      {draft.source === "team_finished" && (
        <label className="teams-field">
          <span className="teams-label">From team</span>
          <select
            className="teams-select"
            value={draft.fromTeam}
            onChange={(event) => onChange({ ...draft, fromTeam: event.target.value })}
          >
            <option value="">Choose a team…</option>
            {draft.fromTeam !== "" && !fromKnown && <option value={draft.fromTeam}>{draft.fromTeam}</option>}
            {otherTeams.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
      )}
      {draft.source === "email_triaged" && (
        <label className="teams-field">
          <span className="teams-label">Email class</span>
          <input
            className="teams-input"
            value={draft.emailClass}
            onChange={(event) => onChange({ ...draft, emailClass: event.target.value })}
          />
        </label>
      )}
      <label className="teams-field">
        <span className="teams-label" data-required="true">
          Request
        </span>
        <textarea
          className="teams-textarea"
          aria-required="true"
          rows={3}
          value={draft.request}
          onChange={(event) => onChange({ ...draft, request: event.target.value })}
        />
      </label>
      <div className="teams-actions">
        <Button type="submit" intent="go" disabled={!valid || create.isPending}>
          {create.isPending ? "Writing…" : "Write rule"}
        </Button>
        {!valid && !create.isPending && (
          <span className="teams-note">
            Add a name, a request{draft.source === "cron" ? " and a schedule" : ""} to write this rule
          </span>
        )}
      </div>
      {create.isError && <CreateRefusal what="write this rule" error={create.error} />}
    </form>
  );
}

function CreateRefusal({ what, error }: { what: string; error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>Couldn't reach NucleOS — couldn't {what}. Try again.</ErrorNote>;
}
