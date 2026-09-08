import { useEffect, useState } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  scopeEngaged,
  useScopedKills,
  useSetScopedKill,
} from "../data/autopilot";
import {
  isAggregateTimeout,
  useApiTokens,
  useBackups,
  useBudget,
  useCalendarConfig,
  useEmailConfig,
  useMintToken,
  usePiiTally,
  useProjects,
  useRevokeToken,
  useSetBudget,
  useSidecars,
  useStageRestore,
  useSystemHealth,
  useTakeBackup,
  useVoiceConfig,
  type ApiTokenLevel,
  type ApiTokenSummary,
  type BackupInfo,
  type BudgetChange,
  type BudgetView,
  type CalendarConfig,
  type CreatedApiToken,
  type EmailConfig,
  type HealthReadout,
  type PiiTallyRow,
  type SidecarState,
  type SubsystemReadout,
  type VoiceConfig,
} from "../data/system";
import {
  Badge,
  Button,
  ConfirmButton,
  CopyOnce,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  StateBadge,
  Section,
} from "../ui";
import "./system.css";

/**
 * System — the machine's own state, not a project's.
 *
 * Three tabs. S1 and S2 built the health view (the daemon's own subsystem
 * readout and the sidecars' own liveness) plus, on top of it, the project
 * brakes and the editable budget; and the whole backups view (snapshots,
 * staged restore and the PII tally). Health data is read off
 * `data/system.ts`'s `useSystemHealth`/`useSidecars` — the ONE health query in
 * the app, which every pillar's own health reading (Browser's included)
 * narrows rather than asking again.
 *
 * This packet (S3) builds the tokens view: minting and revoking API tokens
 * with a show-once secret, plus a config index reading the three `/config/*`-
 * shaped routes that actually exist and naming the four areas that have none.
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
  return (VIEWS as readonly string[]).includes(candidate)
    ? (candidate as SystemView)
    : "health";
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
        {view === "health" && (
          <HealthView health={health} sidecars={sidecars} />
        )}
        {view === "backups" && <BackupsView />}
        {view === "tokens" && <TokensView />}
      </div>
    </>
  );
}

/**
 * How the daemon is, in one sentence.
 *
 * Exported because Home reads it too, on its fifth stat card: the first screen is where
 * somebody finds out that a subsystem is down, and a reading that lives only on the page
 * you go to when you already suspect something is the wrong way round. One function, so
 * the two screens cannot describe the same readout differently.
 */
export function headlineFor(readout: HealthReadout | undefined): string | undefined {
  if (readout === undefined) return undefined;
  if (isAggregateTimeout(readout))
    return "the health readout timed out before it measured anything";
  const down = readout.subsystems.filter((row) => row.status === "down").length;
  const degraded = readout.subsystems.filter(
    (row) => row.status === "degraded",
  ).length;
  if (down === 0 && degraded === 0)
    return "every configured subsystem is healthy";
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

function HealthReadoutPanel({
  health,
}: {
  health: ReturnType<typeof useSystemHealth>;
}) {
  return (
    <Panel title="Subsystems">
      {health.data === undefined && !health.isError && (
        <p className="sy-loading">reading…</p>
      )}
      {health.isError && health.data === undefined && (
        <HealthError error={health.error} />
      )}
      {health.data !== undefined && isAggregateTimeout(health.data) && (
        <p className="sy-timeout" role="status">
          The readout timed out before it could measure anything below the
          aggregate — the ten subsystems below were never reached, which is not
          the same as nine of them being down.
        </p>
      )}
      {health.data !== undefined && !isAggregateTimeout(health.data) && (
        <ul className="ui-rows" aria-label="Subsystems">
          {[...health.data.subsystems].sort(bySubsystemHealth).map((row) => (
            <SubsystemRow key={row.name} row={row} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function SubsystemRow({ row }: { row: SubsystemReadout }) {
  return (
    <li className="ui-rows-row sy-subsystem-row">
      <span className="sy-subsystem-name">{row.name}</span>
      <StateBadge domain="pillar" state={row.status} />
      {row.reason !== undefined && (
        <span className="sy-meta">reason: {row.reason}</span>
      )}
    </li>
  );
}

function HealthError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about the machine&apos;s
      health
    </ErrorNote>
  );
}

function SidecarCardsPanel({
  sidecars,
}: {
  sidecars: ReturnType<typeof useSidecars>;
}) {
  if (sidecars.data !== undefined && sidecars.data.length === 0) {
    return <Section label="Sidecars"><Quiet says="no sidecar is registered." /></Section>;
  }

  return (
    <Panel title="Sidecars">
      {sidecars.data === undefined && !sidecars.isError && (
        <p className="sy-loading">reading…</p>
      )}
      {sidecars.isError && sidecars.data === undefined && (
        <ErrorNote>
          the núcleo did not answer — nothing is known about the sidecars
        </ErrorNote>
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
        A project brake holds what that project would START; it stops nothing
        already running. An absent row means the daemon was never told, which
        reads as released. The trigger brakes — scheduled rules, repo triggers,
        e-mail triage — live on <Link to="/autopilot">Autopilot</Link>.
      </p>
      {kills.isError && kills.data === undefined && (
        <SystemListError error={kills.error} what="the project brakes" />
      )}
      {!kills.isError && kills.data === undefined && (
        <p className="sy-loading">reading the project brakes…</p>
      )}
      {projects.isError && projects.data === undefined && (
        <SystemListError error={projects.error} what="the projects" />
      )}
      {!projects.isError && projects.data === undefined && (
        <p className="sy-loading">reading the projects…</p>
      )}
      {projects.data !== undefined && projects.data.length === 0 && (
        <p className="sy-empty">no project is registered.</p>
      )}
      {projects.data !== undefined && projects.data.length > 0 && (
        <ul className="ui-rows" aria-label="Project brakes">
          {projects.data.map((project) => {
            const engaged = scopeEngaged(
              kills.data,
              "project",
              project.project_id,
            );
            return (
              <li
                className="ui-rows-row sy-project-brake-row"
                key={project.project_id}
              >
                <span className="sy-project-brake-name">
                  {project.project_id}
                </span>
                <Badge tone={engaged ? "paused" : "active"}>
                  {engaged ? "held" : "running"}
                </Badge>
                <Button
                  variant="ghost"
                  intent={engaged ? "go" : "stop"}
                  disabled={kills.data === undefined || setKill.isPending}
                  onClick={() =>
                    setKill.mutate({
                      scope_type: "project",
                      scope_id: project.project_id,
                      engaged: !engaged,
                    })
                  }
                >
                  {engaged
                    ? `Release ${project.project_id}`
                    : `Hold ${project.project_id}`}
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      {setKill.isError && (
        <ErrorNote>
          that brake was not changed — the núcleo refused or did not answer
        </ErrorNote>
      )}
    </Panel>
  );
}

function bySubsystemHealth(
  left: SubsystemReadout,
  right: SubsystemReadout,
): number {
  const rank = (status: SubsystemReadout["status"]): number =>
    status === "down" ? 0 : status === "degraded" ? 1 : 2;
  return rank(left.status) - rank(right.status);
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
    hourlyLimit:
      budget.hourly_limit_usd === null ? "" : String(budget.hourly_limit_usd),
    perRunReserve: String(budget.per_run_reserve_usd),
    timeCost: String(budget.time_cost_per_hour_usd),
  };
}

type CeilingResult = { ok: true; value: number | null } | { ok: false };

/**
 * A blank box is `null` — no ceiling — never `0`; a typed `0` is a real
 * ceiling of zero. Anything that is not a finite number is rejected rather
 * than coerced: `Number("abc")` is `NaN` and `Number("Infinity")` is
 * `Infinity`, and `JSON.stringify` writes both of those as `null` — the wire
 * value for "no ceiling" — which would silently remove a spend ceiling.
 */
function parseCeiling(raw: string): CeilingResult {
  const trimmed = raw.trim();
  if (trimmed === "") return { ok: true, value: null };
  const value = Number(trimmed);
  return Number.isFinite(value) ? { ok: true, value } : { ok: false };
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
  const [badCeilings, setBadCeilings] = useState<string[]>([]);

  useEffect(() => {
    if (budget.data !== undefined && form === null) {
      setForm(formFromBudget(budget.data));
    }
  }, [budget.data, form]);

  return (
    <Panel title="Budget">
      {budget.isError && budget.data === undefined && (
        <SystemListError error={budget.error} what="the budget" />
      )}
      {!budget.isError && budget.data === undefined && (
        <p className="sy-loading">reading the budget…</p>
      )}
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
            <label htmlFor="sy-budget-window-limit">
              Window limit (USD, blank = no ceiling)
            </label>
            <input
              id="sy-budget-window-limit"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              placeholder="no ceiling"
              value={form.windowLimit}
              onChange={(event) => {
                setForm({ ...form, windowLimit: event.target.value });
                setBadCeilings([]);
              }}
            />
            <p className="sy-field-hint">
              currently:{" "}
              {form.windowLimit.trim() === ""
                ? "no ceiling"
                : `$${form.windowLimit}`}
            </p>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-period">Period</label>
            <select
              id="sy-budget-period"
              className="sy-field-input"
              value={form.period}
              onChange={(event) =>
                setForm({
                  ...form,
                  period: event.target.value as BudgetView["period"],
                })
              }
            >
              {BUDGET_PERIODS.map((period) => (
                <option key={period} value={period}>
                  {period}
                </option>
              ))}
            </select>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-hourly-limit">
              Hourly limit (USD, blank = no ceiling)
            </label>
            <input
              id="sy-budget-hourly-limit"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              placeholder="no ceiling"
              value={form.hourlyLimit}
              onChange={(event) => {
                setForm({ ...form, hourlyLimit: event.target.value });
                setBadCeilings([]);
              }}
            />
            <p className="sy-field-hint">
              currently:{" "}
              {form.hourlyLimit.trim() === ""
                ? "no ceiling"
                : `$${form.hourlyLimit}`}
            </p>
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-per-run-reserve">
              Per-run reserve (USD)
            </label>
            <input
              id="sy-budget-per-run-reserve"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              value={form.perRunReserve}
              onChange={(event) =>
                setForm({ ...form, perRunReserve: event.target.value })
              }
            />
          </div>
          <div className="sy-field">
            <label htmlFor="sy-budget-time-cost">
              Time cost per hour (USD)
            </label>
            <input
              id="sy-budget-time-cost"
              className="sy-field-input"
              type="text"
              inputMode="decimal"
              value={form.timeCost}
              onChange={(event) =>
                setForm({ ...form, timeCost: event.target.value })
              }
            />
          </div>
          <ConfirmButton
            label="Save budget"
            confirmLabel="Send these five fields to the daemon"
            variant="ghost"
            onConfirm={() => {
              const windowLimit = parseCeiling(form.windowLimit);
              const hourlyLimit = parseCeiling(form.hourlyLimit);
              const invalid: string[] = [];
              if (!windowLimit.ok) invalid.push("the window limit");
              if (!hourlyLimit.ok) invalid.push("the hourly limit");
              if (!windowLimit.ok || !hourlyLimit.ok) {
                setBadCeilings(invalid);
                return;
              }
              setBadCeilings([]);
              const change: BudgetChange = {
                limit_usd: windowLimit.value,
                period: form.period,
                hourly_limit_usd: hourlyLimit.value,
                per_run_reserve_usd: parseAmount(form.perRunReserve),
                time_cost_per_hour_usd: parseAmount(form.timeCost),
              };
              setBudget.mutate(change);
            }}
          />
          {badCeilings.length > 0 && (
            <ErrorNote>
              the budget was not sent — {badCeilings.join(" and ")} must be a
              number, or blank for no ceiling
            </ErrorNote>
          )}
          {setBudget.isError && (
            <ErrorNote>
              the budget was not changed — the núcleo refused or did not answer
            </ErrorNote>
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
        Staging a restore changes nothing yet — the swap happens the next time
        the núcleo starts.
      </p>
      <div className="sy-backups-actions">
        <Button
          variant="ghost"
          disabled={takeBackup.isPending}
          onClick={() => takeBackup.mutate()}
        >
          Take a backup now
        </Button>
      </div>
      {takeBackup.isError && (
        <ErrorNote>
          the backup was not taken — the núcleo refused or did not answer
        </ErrorNote>
      )}
      {backups.isError && backups.data === undefined && (
        <SystemListError error={backups.error} what="the backups" />
      )}
      {!backups.isError && backups.data === undefined && (
        <p className="sy-loading">reading the backups…</p>
      )}
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
          {backup.migration_version === null
            ? "unknown"
            : backup.migration_version}
        </span>
      </div>
      <ConfirmButton
        label="Stage a restore"
        confirmLabel={`Restore ${backup.name} on next start`}
        variant="ghost"
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
          conflict:
            "a restore is already staged, or that snapshot name is taken — only one can be pending",
          not_found: "that snapshot is no longer there",
          unprocessable: "the núcleo could not verify that snapshot",
          bad_request: "that snapshot name is not one the núcleo will accept",
        }}
      />
    );
  }
  return (
    <ErrorNote>
      the restore could not be staged — the núcleo did not answer
    </ErrorNote>
  );
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
      {pii.isError && pii.data === undefined && (
        <SystemListError error={pii.error} what="the PII tally" />
      )}
      {!pii.isError && pii.data === undefined && (
        <p className="sy-loading">reading…</p>
      )}
      {pii.data !== undefined && pii.data.length === 0 && (
        <p className="sy-empty">nothing recorded.</p>
      )}
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

/* ------------------------------------------------------------------ tokens -- */

const TOKEN_LEVELS: ApiTokenLevel[] = ["read-only", "run-creating", "admin"];

function TokensView() {
  return (
    <>
      <TokensPanel />
      <ConfigIndex />
    </>
  );
}

/**
 * Mint and revoke API tokens.
 *
 * A minted token's secret lives in this panel's own state, not in the query
 * cache — `useMintToken`'s answer is the only place the value exists, and
 * writing it into the cache would dress a value that can never be refetched
 * as one that could be. `CopyOnce` renders it; `onDismiss` is the whole
 * lifecycle of that state.
 */
function TokensPanel() {
  const tokens = useApiTokens();
  const mintToken = useMintToken();
  const [name, setName] = useState("");
  const [level, setLevel] = useState<ApiTokenLevel>("read-only");
  const [minted, setMinted] = useState<CreatedApiToken | null>(null);

  function handleMint() {
    const trimmed = name.trim();
    if (trimmed === "") return;
    mintToken.mutate(
      { name: trimmed, level },
      {
        onSuccess: (created) => {
          setMinted(created);
          setName("");
          setLevel("read-only");
        },
      },
    );
  }

  return (
    <Panel title="API tokens">
      <p className="sy-note">
        A read-only token cannot read the budget or the kill switch — both sit
        outside its read allowlist. A run-creating token may start work. An
        admin token is everything.
      </p>

      {minted !== null && (
        <CopyOnce
          value={minted.token}
          label={`the new token for ${minted.name}`}
          onDismiss={() => setMinted(null)}
        />
      )}

      <div className="sy-mint-form">
        <div className="sy-field">
          <label htmlFor="sy-token-name">Name</label>
          <input
            id="sy-token-name"
            className="sy-field-input"
            type="text"
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </div>
        <fieldset className="sy-token-levels">
          <legend>Level</legend>
          {TOKEN_LEVELS.map((candidate) => (
            <label key={candidate} className="sy-token-level">
              <input
                type="radio"
                name="sy-token-level"
                value={candidate}
                checked={level === candidate}
                onChange={() => setLevel(candidate)}
              />
              {candidate}
            </label>
          ))}
        </fieldset>
        <Button
          variant="ghost"
          disabled={mintToken.isPending || name.trim() === ""}
          onClick={handleMint}
        >
          Mint token
        </Button>
      </div>
      {mintToken.isError && <MintError error={mintToken.error} />}

      {tokens.isError && tokens.data === undefined && (
        <SystemListError error={tokens.error} what="the API tokens" />
      )}
      {!tokens.isError && tokens.data === undefined && (
        <p className="sy-loading">reading the tokens…</p>
      )}
      {tokens.data !== undefined && tokens.data.length === 0 && (
        <p className="sy-empty">no token has been minted.</p>
      )}
      {tokens.data !== undefined && tokens.data.length > 0 && (
        <ul className="sy-tokens" aria-label="API tokens">
          {tokens.data.map((token) => (
            <TokenRow key={token.name} token={token} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function MintError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          bad_request:
            "a token name is 1–64 characters, letters, digits, hyphen or underscore",
          conflict: "there is already a token with that name",
        }}
      />
    );
  }
  return (
    <ErrorNote>the token was not minted — the núcleo did not answer</ErrorNote>
  );
}

/** One row, with its own `useRevokeToken` instance — each row's pending/error state is its own. */
function TokenRow({ token }: { token: ApiTokenSummary }) {
  const revokeToken = useRevokeToken();

  return (
    <li className="sy-token">
      <div className="sy-token-head">
        <span className="sy-token-name">{token.name}</span>
        <Badge tone="info">{token.level}</Badge>
        <span className="sy-token-meta">
          minted <RelativeTime at={token.created_at} />
        </span>
      </div>
      <ConfirmButton
        label="Revoke"
        confirmLabel={`Revoke ${token.name}`}
        variant="danger"
        disabled={revokeToken.isPending}
        onConfirm={() => revokeToken.mutate(token.name)}
      />
      {revokeToken.isError && <RevokeError error={revokeToken.error} />}
    </li>
  );
}

function RevokeError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{ not_found: "that token is already gone" }}
      />
    );
  }
  return (
    <ErrorNote>
      that token was not revoked — the núcleo did not answer
    </ErrorNote>
  );
}

/* ------------------------------------------------------------------- config -- */

/**
 * Areas with no configuration route at all, verified against `core/src/http.rs`'s
 * `/config/*` set plus the two asymmetric paths above it — `/config/email` is
 * the only `/config/*` route, and nothing serves web, browser, council or
 * models. Named rather than requested: there is nothing to ask for.
 */
const UNCONFIGURED_AREAS = ["web", "browser", "council", "models"] as const;

/**
 * Readouts for the config routes that exist, and an honest list of the ones
 * that do not.
 */
function ConfigIndex() {
  return (
    <>
      <EmailConfigPanel />
      <VoiceConfigPanel />
      <CalendarConfigPanel />
      <UnconfiguredAreasPanel />
    </>
  );
}

function ConfigFact({ term, value }: { term: string; value: string }) {
  return (
    <div className="sy-fact">
      <dt>{term}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function EmailConfigPanel() {
  const email = useEmailConfig();
  return (
    <Panel title="Email configuration">
      {email.isError && email.data === undefined && (
        <SystemListError error={email.error} what="the e-mail configuration" />
      )}
      {!email.isError && email.data === undefined && (
        <p className="sy-loading">reading…</p>
      )}
      {email.data !== undefined && <EmailConfigFacts config={email.data} />}
    </Panel>
  );
}

/**
 * `enabled` and `armed` are rendered as two separate facts on purpose — the
 * daemon's own comment says an enabled-but-unarmed mailbox is a real state:
 * the pillar keeps its retention either way, and triage can be held while
 * mail keeps arriving. `local_triage_disabled` is a reason string when local
 * triage was configured and could not be trusted; `null` means nothing is
 * wrong, not that it is off.
 */
function EmailConfigFacts({ config }: { config: EmailConfig }) {
  return (
    <>
      <div className="sy-config-flags">
        <Badge tone={config.enabled ? "active" : "off"}>
          {config.enabled ? "enabled" : "disabled"}
        </Badge>
        <Badge tone={config.armed ? "active" : "paused"}>
          {config.armed ? "armed" : "unarmed"}
        </Badge>
      </div>
      <dl className="sy-config-facts">
        <ConfigFact term="host" value={config.host} />
        <ConfigFact term="username" value={config.username} />
        <ConfigFact term="mailbox" value={config.mailbox} />
        <ConfigFact
          term="sent mailbox"
          value={config.sent_mailbox ?? "not set"}
        />
        <ConfigFact
          term="poll interval"
          value={`${String(config.poll_interval_secs)}s`}
        />
        <ConfigFact
          term="notify classes"
          value={config.notify_classes.join(", ") || "none"}
        />
        <ConfigFact
          term="digest hour (UTC)"
          value={String(config.digest_hour_utc)}
        />
        <ConfigFact
          term="retain bodies (days)"
          value={String(config.retain_bodies_days)}
        />
      </dl>
      <p className="sy-note">
        {config.local_triage_disabled === null
          ? "local triage: nothing is wrong."
          : `local triage disabled: ${config.local_triage_disabled}`}
      </p>
    </>
  );
}

function VoiceConfigPanel() {
  const voice = useVoiceConfig();
  return (
    <Panel title="Voice configuration">
      {voice.isError && voice.data === undefined && (
        <SystemListError error={voice.error} what="the voice configuration" />
      )}
      {!voice.isError && voice.data === undefined && (
        <p className="sy-loading">reading…</p>
      )}
      {voice.data !== undefined && <VoiceConfigFacts config={voice.data} />}
    </Panel>
  );
}

function VoiceConfigFacts({ config }: { config: VoiceConfig }) {
  return (
    <>
      <div className="sy-config-flags">
        <Badge tone={config.armed ? "active" : "paused"}>
          {config.armed ? "armed" : "unarmed"}
        </Badge>
      </div>
      <dl className="sy-config-facts">
        <ConfigFact term="hotkey" value={config.hotkey} />
        <ConfigFact term="memo hotkey" value={config.memo_hotkey} />
        <ConfigFact
          term="cleanup model"
          value={config.cleanup_model ?? "none configured"}
        />
        <ConfigFact
          term="retain dictations (days)"
          value={String(config.retain_dictations_days)}
        />
        <ConfigFact
          term="max capture (s)"
          value={String(config.max_capture_seconds)}
        />
        <ConfigFact
          term="max body (bytes)"
          value={String(config.max_body_bytes)}
        />
        <ConfigFact term="hints" value={config.hints.join(", ") || "none"} />
      </dl>
    </>
  );
}

function CalendarConfigPanel() {
  const calendar = useCalendarConfig();
  return (
    <Panel title="Calendar configuration">
      {calendar.isError && calendar.data === undefined && (
        <SystemListError
          error={calendar.error}
          what="the calendar configuration"
        />
      )}
      {!calendar.isError && calendar.data === undefined && (
        <p className="sy-loading">reading…</p>
      )}
      {calendar.data !== undefined && (
        <CalendarConfigFacts config={calendar.data} />
      )}
    </Panel>
  );
}

function CalendarConfigFacts({ config }: { config: CalendarConfig }) {
  return (
    <dl className="sy-config-facts">
      <ConfigFact term="default timezone" value={config.default_tz} />
      <ConfigFact
        term="working hours"
        value={`${config.working_hours_start}–${config.working_hours_end}`}
      />
      <ConfigFact
        term="working weekdays"
        value={config.working_weekdays.join(", ")}
      />
    </dl>
  );
}

/**
 * The four areas the núcleo exposes no configuration route for at all — named
 * plainly, the same way the Teams page names its missing routes, rather than
 * drawing a request that would only 404.
 */
function UnconfiguredAreasPanel() {
  return (
    <Panel title="Not exposed by the núcleo">
      <p className="sy-note">
        These areas have no configuration route in the núcleo — there is nothing
        here to read or write, and this page does not ask.
      </p>
      <ul
        className="sy-unconfigured"
        aria-label="Areas with no configuration route"
      >
        {UNCONFIGURED_AREAS.map((area) => (
          <li key={area} className="sy-unconfigured-area">
            {area}
          </li>
        ))}
      </ul>
    </Panel>
  );
}

/* ------------------------------------------------------------------- shared -- */

function SystemListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the núcleo did not answer — nothing is known about {what}
    </ErrorNote>
  );
}
