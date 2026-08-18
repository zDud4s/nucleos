import { useEffect, useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import { scopeEngaged, useScopedKills, useSetScopedKill } from "../data/autopilot";
import {
  isAggregateTimeout,
  useBackups,
  useBudget,
  usePiiTally,
  useProjects,
  useSetBudget,
  useSidecars,
  useStageRestore,
  useSystemHealth,
  useTakeBackup,
  type BackupInfo,
  type BudgetChange,
  type BudgetView,
  type HealthReadout,
  type PiiTallyRow,
  type SidecarState,
  type SubsystemReadout,
} from "../data/system";
import { Badge, Button, ConfirmButton, ErrorNote, PageHeader, Panel, RefusalNote, RelativeTime, StateBadge, Teach } from "../ui";
import "./system.css";

/**
 * System — the machine's own state, not a project's.
 *
 * Three tabs. This slice builds the health view (the daemon's own subsystem
 * readout and the sidecars' own liveness) plus, on top of it, the project
 * brakes and the editable budget; and the whole backups view (snapshots,
 * staged restore and the PII tally). Health data is read off
 * `data/system.ts`'s `useSystemHealth`/`useSidecars` — the ONE health query in
 * the app, which every pillar's own health reading (Browser's included)
 * narrows rather than asking again.
 *
 * Tokens arrive with the packet that builds it (S3). This page says so
 * plainly rather than drawing an empty panel that looks broken.
 */

/** The three tabs, in the order they read. */
const VIEWS = ["health", "backups", "tokens"] as const;
export type SystemView = (typeof VIEWS)[number];

const VIEW_LABEL: Record<SystemView, string> = {
  health: "Health",
  backups: "Backups",
  tokens: "Tokens",
};

/**
 * A `$view` param as one of the three.
 *
 * Falls back to `health` rather than refusing: a route parameter is a string,
 * anybody can type one, and a typo in a path is not a missing page.
 */
export function normaliseView(raw: string | undefined): SystemView {
  const candidate = (raw ?? "").trim().toLowerCase();
  return (VIEWS as readonly string[]).includes(candidate) ? (candidate as SystemView) : "health";
}

export function System() {
  const params = useParams({ strict: false }) as { view?: string };
  const view = normaliseView(params.view);
  const health = useSystemHealth();
  const sidecars = useSidecars();

  return (
    <>
      <PageHeader title="System" headline={headlineFor(health.data)} />
      <ViewTabs view={view} />
      <div className="sy-sections">
        {view === "health" && <HealthView health={health} sidecars={sidecars} />}
        {view === "backups" && <BackupsView />}
        {view === "tokens" && <ArrivesLater />}
      </div>
    </>
  );
}

function headlineFor(readout: HealthReadout | undefined): string | undefined {
  if (readout === undefined) return undefined;
  if (isAggregateTimeout(readout)) return "the health readout timed out before it measured anything";
  const down = readout.subsystems.filter((row) => row.status === "down").length;
  const degraded = readout.subsystems.filter((row) => row.status === "degraded").length;
  if (down === 0 && degraded === 0) return "every configured subsystem is healthy";
  const parts: string[] = [];
  if (down > 0) parts.push(`${String(down)} down`);
  if (degraded > 0) parts.push(`${String(degraded)} degraded`);
  return parts.join(", ");
}

function ViewTabs({ view }: { view: SystemView }) {
  return (
    <nav className="sy-tabs" aria-label="System views">
      {VIEWS.map((candidate) => (
        <Link
          key={candidate}
          className={candidate === view ? "sy-tab sy-tab-active" : "sy-tab"}
          to={`/system/${candidate}`}
          aria-current={candidate === view ? "page" : undefined}
        >
          {VIEW_LABEL[candidate]}
        </Link>
      ))}
    </nav>
  );
}

/* ------------------------------------------------------------------ health -- */

function HealthView({
  health,
  sidecars,
}: {
  health: ReturnType<typeof useSystemHealth>;
  sidecars: ReturnType<typeof useSidecars>;
}) {
  return (
    <>
      <HealthReadoutPanel health={health} />
      <SidecarCardsPanel sidecars={sidecars} />
      <ScopedKillsPanel />
      <BudgetPanel />
    </>
  );
}

function HealthReadoutPanel({ health }: { health: ReturnType<typeof useSystemHealth> }) {
  return (
    <Panel title="Subsystems">
      {health.data === undefined && !health.isError && <p className="sy-loading">reading…</p>}
      {health.isError && health.data === undefined && <HealthError error={health.error} />}
      {health.data !== undefined && isAggregateTimeout(health.data) && (
        <p className="sy-timeout" role="status">
          The readout timed out before it could measure anything below the aggregate — the ten
          subsystems below were never reached, which is not the same as nine of them being down.
        </p>
      )}
      {health.data !== undefined && !isAggregateTimeout(health.data) && (
        <ul className="sy-subsystems" aria-label="Subsystems">
          {health.data.subsystems.map((row) => (
            <SubsystemRow key={row.name} row={row} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SubsystemRow({ row }: { row: SubsystemReadout }) {
  return (
    <li className="sy-subsystem">
      <span className="sy-subsystem-name">{row.name}</span>
      <StateBadge domain="pillar" state={row.status} />
      {row.reason !== undefined && <span className="sy-meta">reason: {row.reason}</span>}
    </li>
  );
}

function HealthError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the machine&apos;s health</ErrorNote>;
}

function SidecarCardsPanel({ sidecars }: { sidecars: ReturnType<typeof useSidecars> }) {
  return (
    <Panel title="Sidecars">
      {sidecars.data === undefined && !sidecars.isError && <p className="sy-loading">reading…</p>}
      {sidecars.isError && sidecars.data === undefined && (
        <ErrorNote>the núcleo did not answer — nothing is known about the sidecars</ErrorNote>
      )}
      {sidecars.data !== undefined && sidecars.data.length === 0 && (
        <p className="sy-empty">no sidecar is registered.</p>
      )}
      {sidecars.data !== undefined && sidecars.data.length > 0 && (
        <ul className="sy-sidecars" aria-label="Sidecars">
          {sidecars.data.map((row) => (
            <SidecarCard key={row.name} sidecar={row} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SidecarCard({ sidecar }: { sidecar: SidecarState }) {
  return (
    <li className="sy-sidecar">
      <div className="sy-sidecar-head">
        <span className="sy-sidecar-name">{sidecar.name}</span>
        <span className="sy-sidecar-state">{sidecar.state}</span>
      </div>
      <dl className="sy-sidecar-facts">
        {sidecar.started_at !== null && (
          <div className="sy-fact">
            <dt>started</dt>
            <dd>
              <RelativeTime at={sidecar.started_at} />
            </dd>
          </div>
        )}
        <div className="sy-fact">
          <dt>restarts</dt>
          <dd>{sidecar.restarts}</dd>
        </div>
      </dl>
      {sidecar.last_failure !== null && (
        <p className="sy-sidecar-failure" role="alert">
          {sidecar.last_failure}
          {sidecar.last_failure_at !== null && (
            <>
              {" "}
              (<RelativeTime at={sidecar.last_failure_at} />)
            </>
          )}
        </p>
      )}
      {sidecar.last_line !== null && (
        <p className="sy-sidecar-line">
          <code>{sidecar.last_line}</code>
          {sidecar.last_line_at !== null && (
            <>
              {" "}
              (<RelativeTime at={sidecar.last_line_at} />)
            </>
          )}
        </p>
      )}
    </li>
  );
}

/* ------------------------------------------------------------- not yet built -- */

/**
 * The one honest sentence a not-yet-built tab needs.
 *
 * Not the whole-page {@link Placeholder} — the tab, the header and the rest of
 * the shell around it are all real; only this panel's content is still to
 * come, and it says which packet brings it rather than rendering an empty
 * space that reads as broken.
 */
function ArrivesLater() {
  return (
    <Teach title="Tokens arrives with the next packet">
      <p>This tab is real and will stay — only its content is still to come: token spend and secret access.</p>
    </Teach>
  );
}

/* --------------------------------------------------------- project brakes -- */

/**
 * The per-project kill brakes.
 *
 * Renders **only** `("project", id)` scopes — one row per project from
 * `useProjects()`. The trigger brakes (scheduled rules, repo triggers, e-mail
 * triage) stay on Autopilot, and the global switch lives in the frame's
 * footer on every page; a kill switch belongs in exactly one place, and
 * duplicating one here is the specific failure the design names.
 */
function ScopedKillsPanel() {
  const projects = useProjects();
  const kills = useScopedKills();
  const setKill = useSetScopedKill();

  return (
    <Panel title="Project brakes">
      <p className="sy-note">
        A project brake holds what that project would START; it stops nothing already running. An
        absent row means the daemon was never told, which reads as released. The trigger brakes —
        scheduled rules, repo triggers, e-mail triage — live on <Link to="/autopilot">Autopilot</Link>.
      </p>
      {kills.isError && kills.data === undefined && <SystemListError error={kills.error} what="the project brakes" />}
      {!kills.isError && kills.data === undefined && <p className="sy-loading">reading the project brakes…</p>}
      {projects.isError && projects.data === undefined && (
        <SystemListError error={projects.error} what="the projects" />
      )}
      {!projects.isError && projects.data === undefined && <p className="sy-loading">reading the projects…</p>}
      {projects.data !== undefined && projects.data.length === 0 && (
        <p className="sy-empty">no project is registered.</p>
      )}
      {projects.data !== undefined && projects.data.length > 0 && (
        <ul className="sy-kills" aria-label="Project brakes">
          {projects.data.map((project) => {
            const engaged = scopeEngaged(kills.data, "project", project.project_id);
            return (
              <li className="sy-kill" key={project.project_id}>
                <span className="sy-kill-name">{project.project_id}</span>
                <Badge tone={engaged ? "paused" : "active"}>{engaged ? "held" : "running"}</Badge>
                <Button
                  variant="ghost"
                  intent={engaged ? "go" : "stop"}
                  disabled={kills.data === undefined || setKill.isPending}
                  onClick={() =>
                    setKill.mutate({ scope_type: "project", scope_id: project.project_id, engaged: !engaged })
                  }
                >
                  {engaged ? `Release ${project.project_id}` : `Hold ${project.project_id}`}
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      {setKill.isError && (
        <ErrorNote>that brake was not changed — the núcleo refused or did not answer</ErrorNote>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------------ budget -- */

const BUDGET_PERIODS: BudgetView["period"][] = ["daily", "weekly", "monthly"];

interface BudgetFormState {
  windowLimit: string;
  period: BudgetView["period"];
  hourlyLimit: string;
  perRunReserve: string;
  timeCost: string;
}

function formFromBudget(budget: BudgetView): BudgetFormState {
  return {
    windowLimit: budget.limit_usd === null ? "" : String(budget.limit_usd),
    period: budget.period,
    hourlyLimit: budget.hourly_limit_usd === null ? "" : String(budget.hourly_limit_usd),
    perRunReserve: String(budget.per_run_reserve_usd),
    timeCost: String(budget.time_cost_per_hour_usd),
  };
}

/** A blank box is `null` — no ceiling — never `0`; a typed `0` is a real ceiling of zero. */
function parseCeiling(raw: string): number | null {
  const trimmed = raw.trim();
  return trimmed === "" ? null : Number(trimmed);
}

function parseAmount(raw: string): number {
  const value = Number(raw.trim());
  return Number.isFinite(value) ? value : 0;
}

/**
 * The budget, editable.
 *
 * `BudgetRequest` is a full replace of five fields (C5) — there is no partial
 * update, so every submit sends all five, pre-filled from the last value the
 * daemon confirmed. The form seeds itself from `useBudget()` exactly once
 * (`form === null` guard): the query polls at `POLL.fast`, and re-seeding on
 * every tick would erase whatever a person is in the middle of typing.
 */
function BudgetPanel() {
  const budget = useBudget();
  const setBudget = useSetBudget();
  const [form, setForm] = useState<BudgetFormState | null>(null);

  useEffect(() => {
    if (budget.data !== undefined && form === null) {
      setForm(formFromBudget(budget.data));
    }
  }, [budget.data, form]);

  return (
    <Panel title="Budget">
      {budget.isError && budget.data === undefined && <SystemListError error={budget.error} what="the budget" />}
      {!budget.isError && budget.data === undefined && <p className="sy-loading">reading the budget…</p>}
      {budget.data !== undefined && budget.data.paused && (
        <p className="sy-budget-paused" role="status">
          Autonomous work is currently held: {budget.data.reason}
        </p>
      )}
      {budget.data !== undefined && (
        <dl className="sy-budget-spend">
          <div className="sy-fact">
            <dt>window spend</dt>
            <dd>${budget.data.window_spend_usd.toFixed(2)}</dd>
          </div>
          <div className="sy-fact">
            <dt>hourly spend</dt>
            <dd>${budget.data.hourly_spend_usd.toFixed(2)}</dd>
          </div>
        </dl>
      )}
      {form !== null && (
        <div className="sy-budget-form">
          <div className="sy-field">
            <label htmlFor="sy-budget-window-limit">Window limit (USD, blank = no ceiling)</label>
            <input
              id="sy-budget-window-limit"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              placeholder="no ceiling"
              value={form.windowLimit}
              onChange={(event) => setForm({ ...form, windowLimit: event.target.value })}
            />
            <p className="sy-field-hint">
              currently: {form.windowLimit.trim() === "" ? "no ceiling" : `$${form.windowLimit}`}
            </p>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-period">Period</label>
            <select
              id="sy-budget-period"
              className="sy-field-input"
              value={form.period}
              onChange={(event) => setForm({ ...form, period: event.target.value as BudgetView["period"] })}
            >
              {BUDGET_PERIODS.map((period) => (
                <option key={period} value={period}>
                  {period}
                </option>
              ))}
            </select>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-hourly-limit">Hourly limit (USD, blank = no ceiling)</label>
            <input
              id="sy-budget-hourly-limit"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              placeholder="no ceiling"
              value={form.hourlyLimit}
              onChange={(event) => setForm({ ...form, hourlyLimit: event.target.value })}
            />
            <p className="sy-field-hint">
              currently: {form.hourlyLimit.trim() === "" ? "no ceiling" : `$${form.hourlyLimit}`}
            </p>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-per-run-reserve">Per-run reserve (USD)</label>
            <input
              id="sy-budget-per-run-reserve"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              value={form.perRunReserve}
              onChange={(event) => setForm({ ...form, perRunReserve: event.target.value })}
            />
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-time-cost">Time cost per hour (USD)</label>
            <input
              id="sy-budget-time-cost"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              value={form.timeCost}
              onChange={(event) => setForm({ ...form, timeCost: event.target.value })}
            />
          </div>
          <ConfirmButton
            label="Save budget"
            confirmLabel="Send these five fields to the daemon"
            variant="ghost"
            onConfirm={() => {
              const change: BudgetChange = {
                limit_usd: parseCeiling(form.windowLimit),
                period: form.period,
                hourly_limit_usd: parseCeiling(form.hourlyLimit),
                per_run_reserve_usd: parseAmount(form.perRunReserve),
                time_cost_per_hour_usd: parseAmount(form.timeCost),
              };
              setBudget.mutate(change);
            }}
          />
          {setBudget.isError && (
            <ErrorNote>the budget was not changed — the núcleo refused or did not answer</ErrorNote>
          )}
        </div>
      )}
    </Panel>
  );
}

/* ------------------------------------------------------------------ backups -- */

function BackupsView() {
  return (
    <>
      <BackupsPanel />
      <PiiObservations />
    </>
  );
}

/**
 * Snapshots, taking one, and staging a restore.
 *
 * Restoring changes nothing immediately: the daemon only stages the swap, and
 * `StagedRestore.applies` is its own sentence about when the swap actually
 * happens (its next start) — rendered verbatim rather than paraphrased (C3).
 */
function BackupsPanel() {
  const backups = useBackups();
  const takeBackup = useTakeBackup();

  return (
    <Panel title="Backups">
      <p className="sy-note">
        Staging a restore changes nothing yet — the swap happens the next time the núcleo starts.
      </p>
      <div className="sy-backups-actions">
        <Button variant="ghost" disabled={takeBackup.isPending} onClick={() => takeBackup.mutate()}>
          Take a backup now
        </Button>
      </div>
      {takeBackup.isError && (
        <ErrorNote>the backup was not taken — the núcleo refused or did not answer</ErrorNote>
      )}
      {backups.isError && backups.data === undefined && <SystemListError error={backups.error} what="the backups" />}
      {!backups.isError && backups.data === undefined && <p className="sy-loading">reading the backups…</p>}
      {backups.data !== undefined && backups.data.length === 0 && (
        <p className="sy-empty">no backup has been taken yet.</p>
      )}
      {backups.data !== undefined && backups.data.length > 0 && (
        <ul className="sy-backups" aria-label="Backups">
          {backups.data.map((backup) => (
            <BackupRow key={backup.name} backup={backup} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function BackupRow({ backup }: { backup: BackupInfo }) {
  const stageRestore = useStageRestore();

  return (
    <li className="sy-backup">
      <div className="sy-backup-head">
        <span className="sy-backup-name">{backup.name}</span>
        <span className="sy-backup-meta">
          {formatBytes(backup.size_bytes)} · migration{" "}
          {backup.migration_version === null ? "unknown" : backup.migration_version}
        </span>
      </div>
      <ConfirmButton
        label="Stage a restore"
        confirmLabel={`Restore ${backup.name} on next start`}
        disabled={stageRestore.isPending}
        onConfirm={() => stageRestore.mutate(backup.name)}
      />
      {stageRestore.isSuccess && stageRestore.data !== undefined && (
        <p className="sy-restore-applied" role="status">
          Nothing has changed yet — {stageRestore.data.applies}
        </p>
      )}
      {stageRestore.isError && <RestoreError error={stageRestore.error} />}
    </li>
  );
}

function RestoreError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          conflict: "a restore is already staged, or that snapshot name is taken — only one can be pending",
          not_found: "that snapshot is no longer there",
          unprocessable: "the núcleo could not verify that snapshot",
          bad_request: "that snapshot name is not one the núcleo will accept",
        }}
      />
    );
  }
  return <ErrorNote>the restore could not be staged — the núcleo did not answer</ErrorNote>;
}

/** Bytes, for a person — the same three-step scale `MailDetail.tsx` and `data/files.ts` use. */
function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

/* -------------------------------------------------------------------- pii -- */

/**
 * The PII tally: how many observations of each class, by column.
 *
 * An empty tally reads as "nothing recorded" — not the same claim as the
 * sweep never having run, which this data does not support making.
 */
function PiiObservations() {
  const pii = usePiiTally();

  return (
    <Panel title="PII observations">
      {pii.isError && pii.data === undefined && <SystemListError error={pii.error} what="the PII tally" />}
      {!pii.isError && pii.data === undefined && <p className="sy-loading">reading…</p>}
      {pii.data !== undefined && pii.data.length === 0 && <p className="sy-empty">nothing recorded.</p>}
      {pii.data !== undefined && pii.data.length > 0 && (
        <table className="sy-pii-table">
          <thead>
            <tr>
              <th scope="col">Column</th>
              <th scope="col">Class</th>
              <th scope="col">Count</th>
            </tr>
          </thead>
          <tbody>
            {pii.data.map((row) => (
              <PiiTallyRowView key={`${row.column}-${row.class}`} row={row} />
            ))}
          </tbody>
        </table>
      )}
    </Panel>
  );
}

function PiiTallyRowView({ row }: { row: PiiTallyRow }) {
  return (
    <tr>
      <td>{row.column}</td>
      <td>{row.class}</td>
      <td>{row.count}</td>
    </tr>
  );
}

/* ------------------------------------------------------------------- shared -- */

function SystemListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about {what}</ErrorNote>;
}
