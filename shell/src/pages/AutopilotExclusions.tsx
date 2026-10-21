import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import {
  useExclusions,
  useLiveJobs,
  useProposeExclusion,
  useRevokeExclusion,
  type FleetExclusion,
  type Job,
} from "../data/fleet";
import { Button, ConfirmButton, ErrorNote, Field, Inset, Panel, Quiet, RefusalNote, RelativeTime } from "../ui";

/**
 * Which jobs are kept apart, and the way to keep two more apart.
 *
 * This is where the Fleet page's exclusions went when it became the live agent tree: the list of
 * rules in force (with Lift) and the "Never at the same time as" question. Asking only files a
 * request — nothing changes until it is answered in Waiting — and lifting takes effect at once,
 * which is why it sits behind the interlock.
 */
export function ExclusionsPanel() {
  const rules = useExclusions();
  const jobs = useLiveJobs();
  const revoke = useRevokeExclusion();
  const active = rules.data ?? [];

  return (
    <Panel title="Never at the same time">
      {rules.isError && rules.data === undefined && (
        <ErrorNote>Could not reach the {DAEMON_NAME}: nothing is known about the exclusions.</ErrorNote>
      )}
      {rules.data !== undefined && active.length === 0 && <Quiet says="No two jobs are kept apart." />}
      {active.length > 0 && (
        <ul className="ap-list" aria-label="Exclusions in force">
          {active.map((rule) => (
            <ExclusionRow key={rule.id} rule={rule} onLift={() => revoke.mutate(rule.id)} />
          ))}
        </ul>
      )}
      {revoke.isError && <ErrorNote>Could not reach the {DAEMON_NAME}: the rule was not lifted.</ErrorNote>}
      <KeepApart jobs={jobs.data ?? []} rules={active} />
    </Panel>
  );
}

function ExclusionRow({ rule, onLift }: { rule: FleetExclusion; onLift: () => void }) {
  return (
    <Inset as="li" className="ap-job">
      <div className="ap-card-head">
        <span className="ap-card-id">
          job {rule.job_low} · job {rule.job_high}
        </span>
        <span className="ap-meta">{rule.project_id}</span>
        <RelativeTime at={rule.created_at} />
      </div>
      {/* Only the higher id waits: the daemon parks it while the lower one holds a slot. */}
      <p className="ap-meta">never at the same time: job {rule.job_high} waits</p>
      <div className="ap-actions">
        <ConfirmButton
          label="Lift"
          confirmLabel="Lift the rule"
          sayAs={`lifts the rule, so job ${rule.job_low} and job ${rule.job_high} may run at the same time`}
          variant="quiet"
          onConfirm={onLift}
        />
      </div>
    </Inset>
  );
}

/**
 * The pairing question. Both ends must be live jobs of one project (the daemon answers anything
 * else with a 400), and a pair that already has a rule is not offered again.
 */
function KeepApart({ jobs, rules }: { jobs: Job[]; rules: FleetExclusion[] }) {
  const propose = useProposeExclusion();
  const [chosenFirst, setFirst] = useState<number | null>(null);
  const [chosenSecond, setSecond] = useState<number | null>(null);

  const joined = (x: number, y: number) =>
    rules.some((rule) => rule.job_low === Math.min(x, y) && rule.job_high === Math.max(x, y));
  const hasPartner = (job: Job): boolean =>
    jobs.some((other) => other.id !== job.id && other.project_id === job.project_id && !joined(job.id, other.id));
  // Only a job that still has a valid partner is offered as the first end: a lone job of another
  // project must not hide the form. A job can also leave between two polls, so fall back to the
  // first candidate left rather than send a pair naming a job that is gone.
  const candidates = jobs.filter(hasPartner);
  const first = candidates.find((job) => job.id === chosenFirst) ?? candidates[0];
  const partners =
    first === undefined
      ? []
      : jobs.filter((other) => other.id !== first.id && other.project_id === first.project_id && !joined(first.id, other.id));
  const second = partners.find((job) => job.id === chosenSecond) ?? partners[0];

  if (first === undefined || second === undefined) {
    return <p className="ap-meta">Asking needs two live jobs in the same project that are not already kept apart.</p>;
  }
  return (
    <form
      className="ap-form ap-pair"
      onSubmit={(event) => {
        event.preventDefault();
        propose.mutate({ job_a: first.id, job_b: second.id, paths: [] });
      }}
    >
      <Field label="Job">
        <select value={first.id} onChange={(event) => setFirst(Number(event.target.value))}>
          {candidates.map((job) => (
            <option key={job.id} value={job.id}>
              job {job.id} · {job.project_id}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Never at the same time as">
        <select value={second.id} onChange={(event) => setSecond(Number(event.target.value))}>
          {partners.map((job) => (
            <option key={job.id} value={job.id}>
              job {job.id}
            </option>
          ))}
        </select>
      </Field>
      <Button type="submit" disabled={propose.isPending}>
        Ask
      </Button>
      {propose.isSuccess && (
        <p className="ap-meta">
          Asked. Nothing changes until it is answered in{" "}
          <Link className="ap-link" to="/waiting">
            Waiting
          </Link>
          .
        </p>
      )}
      {propose.isError &&
        (isApiRefusal(propose.error) ? (
          <RefusalNote refusal={propose.error} />
        ) : (
          <ErrorNote>Could not reach the {DAEMON_NAME}: nothing was asked.</ErrorNote>
        ))}
    </form>
  );
}
