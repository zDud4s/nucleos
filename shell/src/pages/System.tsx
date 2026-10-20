import { useEffect, useState, type ReactNode } from "react";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { validateSystemSearch } from "./systemSearch";
import { isApiRefusal } from "../data/client";
import { formatBytes } from "../data/files";
import { routeTriple, useRouteReport, type RoutePair } from "../data/route";
import { NotchProviders } from "./NotchProviders";
import {
  scopeEngaged,
  useScopedKills,
  useSetScopedKill,
} from "../data/autopilot";
import {
  isAggregateTimeout,
  sidecarKeyOf,
  subsystemCounts,
  useApiTokens,
  useBackups,
  useBudget,
  useCalendarConfig,
  useEmailConfig,
  useMintToken,
  usePiiTally,
  useProjects,
  useQuotaBrake,
  useRevokeToken,
  useRestartSidecar,
  useSetBudget,
  useSetQuotaBrake,
  useSidecars,
  useStageCompaction,
  useStageRestore,
  useStorage,
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
  type QuotaBrakeView,
  type SidecarState,
  type SubsystemReadout,
  type VoiceConfig,
} from "../data/system";
import {
  Badge,
  Button,
  ConfirmButton,
  CopyButton,
  CopyOnce,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  StatCard,
  StateBadge,
  Meter,
} from "../ui";
import { MachineSettings } from "./MachineSettings";
import { ChannelsView } from "./ChannelsView";
import { NotificationsView } from "./NotificationsView";
import "./system.css";
import { DAEMON_NAME } from "../copy";

/**
 * System — the machine's own state, not a project's.
 *
 * Seven tabs. S1 and S2 built the health view (the daemon's own subsystem
 * readout and the sidecars' own liveness) plus, on top of it, the project
 * brakes and the editable budget; and the whole backups view (snapshots,
 * staged restore and the PII tally). Health data is read off
 * `data/system.ts`'s `useSystemHealth`/`useSidecars` — the ONE health query in
 * the app, which every pillar's own health reading (Browser's included)
 * narrows rather than asking again.
 *
 * S3 built the tokens view: minting and revoking API tokens with a show-once
 * secret, plus a config index reading the three `/config/*`-shaped routes that
 * existed — and naming, honestly, the four areas that had no route at all.
 *
 * The settings view is what answers that confession. `GET`/`POST
 * /config/machine` now serve the whole of this machine's settings — the nine
 * files under `~/.nucleos/` whose author is the daemon rather than any project — so every
 * pillar can be configured here instead of in a text editor followed by a
 * restart. The config index above it stays, narrowed to what it is actually
 * good at: showing what the daemon is RUNNING, which is a different reading
 * from what is on disk whenever the two have been allowed to drift.
 *
 * The notifications tab is the newest, and belongs here for the same reason the
 * budget does: it is the machine's own behaviour, not a project's. It edits
 * which feed kinds still reach a channel — a preference the núcleo stores and
 * the sidecar obeys, so nothing on this page decides anything by itself.
 */

/** The seven tabs, in the order they read. */
const VIEWS = ["health", "limits", "backups", "tokens", "channels", "notifications", "settings"] as const;
export type SystemView = (typeof VIEWS)[number];

const VIEW_LABEL: Record<SystemView, string> = {
  health: "Health",
  limits: "Limits",
  backups: "Backups",
  tokens: "Tokens",
  channels: "Channels",
  notifications: "Notifications",
  // The slug stays `settings`; only the word changed, because "System" inside the page
  // called System named nothing.
  settings: "Machine",
};

/**
 * A `$view` param as one of the seven.
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

export { validateSystemSearch };

export function System() {
  const params = useParams({ strict: false }) as { view?: string };
  const view = normaliseView(params.view);
  const { channel } = validateSystemSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const navigate = useNavigate();
  const health = useSystemHealth();
  const sidecars = useSidecars();

  return (
    <>
      <PageHeader title="System" headline={headlineNodeFor(health.data)} />
      <ViewTabs view={view} />
      <div className="sy-sections">
        {view === "health" && (
          <HealthView health={health} sidecars={sidecars} />
        )}
        {view === "limits" && <BudgetPanel />}
        {view === "backups" && <BackupsView />}
        {view === "tokens" && <TokensView />}
        {view === "channels" && <ChannelsView />}
        {view === "notifications" && (
          <NotificationsView
            channel={channel}
            onChannelChange={(next) => {
              void navigate({
                to: "/system/$view",
                params: { view: "notifications" },
                search: (next === undefined ? {} : { channel: next }) as never,
              });
            }}
          />
        )}
        {view === "settings" && (
          <>
            <MachineSettings />
            {/* A preference of this screen, not a limit: it lives with the machine's own settings. */}
            <Section label="Appearance">
              <NotchProviders />
            </Section>
          </>
        )}
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

/** The same sentence as {@link headlineFor}, toned and with a shape when something is wrong. */
export function headlineNodeFor(readout: HealthReadout | undefined): ReactNode {
  const text = headlineFor(readout);
  if (text === undefined) return undefined;
  const wrong =
    readout !== undefined &&
    !isAggregateTimeout(readout) &&
    readout.subsystems.some((row) => row.status === "down" || row.status === "degraded");
  if (!wrong) return text;
  // The triangle is the second signal: colour alone is not a state.
  return (
    <>
      <span aria-hidden="true">▲ </span>
      <span className="ui-wrong">{text}</span>
    </>
  );
}

/**
 * The six views — links, and deliberately **not** `Tabs` from `../ui`.
 *
 * The primitive is the app's one tabs implementation and the reason to reach
 * for it is real: `Bench` gets Radix's roving focus and its `aria-controls`
 * wiring, and a second hand-rolled tab bar is how two of them come to behave
 * differently. It cannot carry this one, and the difference is not cosmetic.
 * These are routes rather than panels: every trigger here is a real `<a href>`
 * to `/system/<view>`, which opens in a new window, is announced as a link, and
 * marks the current one with `aria-current="page"`. Radix's `Trigger` puts
 * `role="tab"` on whatever it renders — `asChild` and a `Link` included — so
 * adopting it would replace the link role, swap `aria-current` for
 * `aria-selected`, collapse three tab stops into one roving one, and leave
 * `aria-controls` pointing at panels that exist only for the route you are
 * already on. A tab widget switches panels inside a page. This switches pages.
 *
 * What was adopted is the rule underneath the appearance: the current tab is
 * marked in `--text`, the way `.ui-tab[data-state="active"]` marks its own.
 * The border used to be `--accent`, which the system reserves for the wordmark,
 * links and the focus ring — a selection wearing the brand colour reads as a
 * status.
 */
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
      <RouterReportPanel />
      <SidecarCardsPanel sidecars={sidecars} />
      {/* Side by side when there is room: neither needs the page's full width,
          and stacked they made the health view three screens long. */}
      <div className="sy-health-pair">
        <ScopedKillsPanel />
        <SpendSummary />
      </div>
    </>
  );
}

/**
 * Spend at a glance, read-only. The forms that change the limits live in the Limits tab: Health
 * answers "is everything fine?", and a nine-field form is not part of that answer.
 */
function SpendSummary() {
  const budget = useBudget();
  return (
    <Panel
      title="Budget"
      aside={<Link to="/system/limits">Edit limits</Link>}
    >
      {budget.isError && budget.data === undefined && (
        <SystemListError error={budget.error} what="the budget" />
      )}
      {!budget.isError && budget.data === undefined && <Loading>reading the budget…</Loading>}
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
    </Panel>
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
        <Loading>reading…</Loading>
      )}
      {health.isError && health.data === undefined && (
        <HealthError error={health.error} />
      )}
      {health.data !== undefined && isAggregateTimeout(health.data) && (
        <p className="sy-timeout" role="status">
          The readout timed out before it could measure anything below the
          aggregate — the subsystems below were never reached, which is not
          the same as all of them being down.
        </p>
      )}
      {health.data !== undefined && !isAggregateTimeout(health.data) && (
        <ul className="sy-subsystems" aria-label="Subsystems">
          {[...health.data.subsystems].sort(bySubsystemHealth).map((row) => (
            <SubsystemRow key={row.name} row={row} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/** One side of a pair as `runner · model · effort`, skipping what is unknown. */
function pairLabel(runner: string | null, model: string | null, effort: string | null): string {
  return routeTriple([runner, model, effort]) ?? "unknown";
}

/**
 * The llm-router over the last thirty days: how much it saw, how often its
 * advice matched what ran, and the pairings that account for the difference.
 *
 * It lives here and not on Runs because it answers a different question. It
 * reports on every routed run of the window, ignores the Runs page's filters,
 * and serves a config decision: is `shadow` ready to become `apply`, per
 * surface, in `router.yaml` (`core/src/route_report.rs`)? That sits beside the
 * subsystems readout and its `llm_router` row, not above a list of runs.
 *
 * The words are the router's own and are spelt out because the Runs page has a
 * run *mode* called "shadow" too (plan mode, no relation): here shadow is "advice
 * only" and apply is "advice applied", with the raw word kept in parentheses so
 * it still matches the file being edited.
 *
 * Nothing at all while loading, on any failure (a 404 from an older daemon is
 * one) or when no run was routed: an empty panel would claim a router that has
 * not spoken.
 */
function RouterReportPanel() {
  const report = useRouteReport();
  const data = report.data;
  // Said, not silent: "the router is not configured" and "the report failed" are different
  // facts, and a page whose thesis is that the safety layer is visible cannot omit either.
  if (report.isError) {
    return <Section label="Router"><Quiet says="the router report is unavailable." /></Section>;
  }
  if (data === undefined) return null;
  if (data.runs <= 0) {
    return <Section label="Router"><Quiet says="no routed runs in the last 30 days." /></Section>;
  }
  // Among shadow runs with advice only: an apply run launched its advice and cannot disagree.
  const shadowAdvised = data.shadow_advised;
  const rate = shadowAdvised > 0 ? `${Math.round((data.matched / shadowAdvised) * 100)}%` : "n/a";
  const pairs: RoutePair[] = data.pairs.slice(0, 5);
  const totals: readonly { label: string; value: string }[] = [
    { label: "Routed runs", value: String(data.runs) },
    { label: "Advice only (shadow)", value: String(data.shadow) },
    { label: "Advice applied (apply)", value: String(data.apply) },
    { label: "With advice", value: String(data.advised) },
    { label: "Match rate", value: rate },
  ];
  return (
    <Panel title={`Router (last ${String(data.days)} days)`}>
      <dl className="sy-router-totals">
        {totals.map((total) => (
          <div key={total.label} className="sy-router-total">
            <dt>{total.label}</dt>
            <dd>{total.value}</dd>
          </div>
        ))}
      </dl>
      {pairs.length > 0 && (
        <table className="sy-router-pairs">
          <caption className="sy-router-caption">Ran → advised</caption>
          <thead>
            <tr>
              <th scope="col">Pair</th>
              <th scope="col">Runs</th>
              <th scope="col">Passed</th>
              <th scope="col">Failed</th>
            </tr>
          </thead>
          <tbody>
            {pairs.map((pair, index) => (
              <tr key={index}>
                <td>{`${pairLabel(pair.runner, pair.model, pair.effort)} → ${pairLabel(pair.advised_runner, pair.advised_model, pair.advised_effort)}`}</td>
                <td>{pair.runs}</td>
                <td>{pair.passed}</td>
                <td>{pair.failed}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Panel>
  );
}

function SubsystemRow({ row }: { row: SubsystemReadout }) {
  const key = sidecarKeyOf(row.name);
  const counts = subsystemCounts(row);
  return (
    <li className="sy-subsystem">
      <div className="sy-subsystem-head">
        <span className="sy-subsystem-name">{row.name}</span>
        <StateBadge domain="pillar" state={row.status} />
      </div>
      {/* Always drawn, empty or not, so every tile is the same two lines tall. */}
      <div className="sy-subsystem-foot">
        <span className="sy-meta sy-subsystem-reason">
          {row.reason !== undefined && <>reason: {row.reason}</>}
        </span>
        {counts.length > 0 && (
          <span className="sy-meta sy-subsystem-counts">
            {counts.map((count) => (
              <span key={count.name}>
                {count.label}{" "}
                {count.instant ? (
                  <RelativeTime at={new Date(count.value * 1000).toISOString()} />
                ) : (
                  count.value
                )}
              </span>
            ))}
          </span>
        )}
        {key !== null && row.status === "down" && (
          <div className="sy-subsystem-action">
            <RestartSidecar name={key} />
          </div>
        )}
      </div>
    </li>
  );
}

function RestartSidecar({ name }: { name: string }) {
  const restart = useRestartSidecar();
  return (
    <>
      <ConfirmButton
        label="Restart"
        /* No `subject`: the tile already names the sidecar, and the subject drawn into the
           armed label made the control wider than the tile it sits in. */
        confirmLabel="Start it again"
        variant="quiet"
        disabled={restart.isPending}
        onConfirm={() => {
          restart.mutate(name);
        }}
      />
      {restart.isSuccess && (
        <span className="sy-restart-asked" role="status">
          asked — the supervisor is trying now
        </span>
      )}
      {restart.isError &&
        (isApiRefusal(restart.error) ? (
          <RefusalNote
            refusal={restart.error}
            sentences={{
              running: "it is running now — there was nothing to start",
              not_supervised:
                "nothing is supervising it — the daemon starts a sidecar only when its pillar is switched on, and only at startup",
              kill_switch:
                "the emergency stop is engaged — release it first; the supervisor keeps retrying on its own meanwhile",
            }}
          />
        ) : (
          <ErrorNote>the {DAEMON_NAME} is not responding — nothing was asked of it</ErrorNote>
        ))}
    </>
  );
}

function HealthError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the {DAEMON_NAME} is not responding — nothing is known about the machine&apos;s
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
        <Loading>reading…</Loading>
      )}
      {sidecars.isError && sidecars.data === undefined && (
        <ErrorNote>
          the {DAEMON_NAME} is not responding — nothing is known about the sidecars
        </ErrorNote>
      )}
      {sidecars.data !== undefined && sidecars.data.length > 0 && (
        <Rows label="Sidecars">
          {sidecars.data.map((row) => (
            <SidecarCard key={row.name} sidecar={row} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

function SidecarCard({ sidecar }: { sidecar: SidecarState }) {
  // The failure and the last line are clipped to one line in the row; this opens them in full.
  // A `title` did that for a mouse only — a keyboard or a finger never saw the rest.
  const [wide, setWide] = useState(false);
  return (
    <Row dense className="sy-sidecar-row">
      <div className="sy-sidecar-head">
        <span className="sy-sidecar-name">{sidecar.name}</span>
        <StateBadge domain="sidecar" state={sidecar.state} />
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
      {/* The third column: what the sidecar last said, and the failure above it when
          there is one — each one clipped line, the whole text in the tooltip. */}
      <div className={wide ? "sy-sidecar-out sy-sidecar-out-wide" : "sy-sidecar-out"}>
        {sidecar.last_failure !== null && (
          <p className="sy-sidecar-failure" role="alert">
            {sidecar.last_failure}
            {sidecar.last_failure_at !== null && (
              <span className="sy-sidecar-when">
                {" "}
                <RelativeTime at={sidecar.last_failure_at} />
              </span>
            )}
          </p>
        )}
        {sidecar.last_line !== null && (
          <p className="sy-sidecar-line">
            <code>{sidecar.last_line}</code>
            {sidecar.last_line_at !== null && (
              <span className="sy-sidecar-when">
                {" "}
                <RelativeTime at={sidecar.last_line_at} />
              </span>
            )}
          </p>
        )}
        {(sidecar.last_failure !== null || sidecar.last_line !== null) && (
          <span className="sy-sidecar-tools">
            <Button variant="quiet" aria-expanded={wide} onClick={() => { setWide(!wide); }}>
              {wide ? "Show less" : "Show all"}
            </Button>
            <CopyButton
              value={[sidecar.last_failure, sidecar.last_line].filter((t) => t !== null).join("\n")}
              label={`Copy ${sidecar.name} output`}
            />
          </span>
        )}
      </div>
    </Row>
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
  const noProjects = projects.data !== undefined && projects.data.length === 0;

  return (
    <Panel title="Project brakes">
      <PanelNote empty={noProjects} says="no project is registered.">
        A project brake holds what that project would START; it stops nothing already running. An
        absent row means the daemon was never told, which reads as released. The trigger brakes —
        scheduled rules, repo triggers, e-mail triage — live on <Link to="/autopilot">Autopilot</Link>.
      </PanelNote>
      {kills.isError && kills.data === undefined && <SystemListError error={kills.error} what="the project brakes" />}
      {!kills.isError && kills.data === undefined && <Loading>reading the project brakes…</Loading>}
      {projects.isError && projects.data === undefined && (
        <SystemListError error={projects.error} what="the projects" />
      )}
      {!projects.isError && projects.data === undefined && <Loading>reading the projects…</Loading>}
      {projects.data !== undefined && projects.data.length > 0 && (
        <ul className="sy-brakes" aria-label="Project brakes">
          {projects.data.map((project) => {
            const engaged = scopeEngaged(
              kills.data,
              "project",
              project.project_id,
            );
            return (
              <li className="sy-brake" key={project.project_id}>
                <span className="sy-project-brake-name" title={project.project_id}>
                  {project.project_id}
                </span>
                <StateBadge domain="brake" state={engaged ? "held" : "released"} />
                <Button
                  variant="ghost"
                  intent={engaged ? "go" : "stop"}
                  /* The tile already names the project; the button says only the verb. */
                  aria-label={
                    engaged ? `Release ${project.project_id}` : `Hold ${project.project_id}`
                  }
                  disabled={kills.data === undefined || setKill.isPending}
                  onClick={() =>
                    setKill.mutate({
                      scope_type: "project",
                      scope_id: project.project_id,
                      engaged: !engaged,
                    })
                  }
                >
                  {engaged ? "Release" : "Hold"}
                </Button>
              </li>
            );
          })}
        </ul>
      )}
      {setKill.isError && (
        <ErrorNote>
          that brake was not changed — the {DAEMON_NAME} refused or is not responding
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
  relayChainLimit: string;
}

function formFromBudget(budget: BudgetView): BudgetFormState {
  return {
    windowLimit: budget.limit_usd === null ? "" : String(budget.limit_usd),
    period: budget.period,
    hourlyLimit:
      budget.hourly_limit_usd === null ? "" : String(budget.hourly_limit_usd),
    perRunReserve: String(budget.per_run_reserve_usd),
    timeCost: String(budget.time_cost_per_hour_usd),
    relayChainLimit: String(budget.relay_chain_limit_usd),
  };
}

interface QuotaBrakeFormState {
  enabled: boolean;
  fiveHour: string;
  sevenDay: string;
}

function formFromQuotaBrake(quotaBrake: QuotaBrakeView): QuotaBrakeFormState {
  return {
    enabled: quotaBrake.enabled,
    fiveHour: String(quotaBrake.pause_above_percent_5h),
    sevenDay: String(quotaBrake.pause_above_percent_7d),
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

function parseQuotaPercent(raw: string): number | null {
  const value = Number(raw.trim());
  return Number.isInteger(value) && value >= 1 && value <= 100 ? value : null;
}

/**
 * The budget, editable.
 *
 * `BudgetRequest` is a full replace of six fields (C5) — there is no partial
 * update, so every submit sends all six, pre-filled from the last value the
 * daemon confirmed. The form seeds itself from `useBudget()` exactly once
 * (`form === null` guard): the query polls at `POLL.fast`, and re-seeding on
 * every tick would erase whatever a person is in the middle of typing.
 */
function BudgetPanel() {
  const budget = useBudget();
  const setBudget = useSetBudget();
  const quotaBrake = useQuotaBrake();
  const setQuotaBrake = useSetQuotaBrake();
  const [form, setForm] = useState<BudgetFormState | null>(null);
  const [badCeilings, setBadCeilings] = useState<string[]>([]);
  const [quotaBrakeForm, setQuotaBrakeForm] = useState<QuotaBrakeFormState | null>(null);
  const [badQuotaBrake, setBadQuotaBrake] = useState(false);
  const [budgetSaved, setBudgetSaved] = useState(false);
  const [quotaSaved, setQuotaSaved] = useState(false);

  useEffect(() => {
    if (budget.data !== undefined && form === null) {
      setForm(formFromBudget(budget.data));
    }
  }, [budget.data, form]);

  useEffect(() => {
    if (quotaBrake.data !== undefined && quotaBrakeForm === null) {
      setQuotaBrakeForm(formFromQuotaBrake(quotaBrake.data));
    }
  }, [quotaBrake.data, quotaBrakeForm]);

  // Leaving with an edit that was never saved loses it silently; the browser's own prompt
  // is the only guard a window close or reload offers.
  const budgetDirty =
    form !== null &&
    budget.data !== undefined &&
    JSON.stringify(form) !== JSON.stringify(formFromBudget(budget.data));
  const quotaDirty =
    quotaBrakeForm !== null &&
    quotaBrake.data !== undefined &&
    JSON.stringify(quotaBrakeForm) !== JSON.stringify(formFromQuotaBrake(quotaBrake.data));
  const unsaved = budgetDirty || quotaDirty;
  useEffect(() => {
    if (!unsaved) return;
    const warn = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [unsaved]);

  return (
    <Panel title="Limits">
      {budget.isError && budget.data === undefined && (
        <SystemListError error={budget.error} what="the budget" />
      )}
      {!budget.isError && budget.data === undefined && (
        <Loading>reading the budget…</Loading>
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
      <div className="sy-budget-groups">
        {form !== null && (
          <section className="sy-budget-group">
            <h3 className="sy-budget-heading">Spend limits</h3>
            <div className="sy-budget-fields">
              <div className="sy-field">
                <label htmlFor="sy-budget-window-limit">Window limit (USD)</label>
                <input
                  id="sy-budget-window-limit"
                  className="sy-field-input"
                  type="text"
                  inputMode="decimal"
                  placeholder="no ceiling"
                  value={form.windowLimit}
                  aria-invalid={badCeilings.includes("the window limit") ? true : undefined}
                  aria-describedby={badCeilings.length > 0 ? "sy-budget-error" : undefined}
                  onChange={(event) => {
                    setForm({ ...form, windowLimit: event.target.value });
                    setBudgetSaved(false);
                    setBadCeilings([]);
                  }}
                />
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
                <label htmlFor="sy-budget-hourly-limit">Hourly limit (USD)</label>
                <input
                  id="sy-budget-hourly-limit"
                  className="sy-field-input"
                  type="text"
                  inputMode="decimal"
                  placeholder="no ceiling"
                  value={form.hourlyLimit}
                  aria-invalid={badCeilings.includes("the hourly limit") ? true : undefined}
                  aria-describedby={badCeilings.length > 0 ? "sy-budget-error" : undefined}
                  onChange={(event) => {
                    setForm({ ...form, hourlyLimit: event.target.value });
                    setBudgetSaved(false);
                    setBadCeilings([]);
                  }}
                />
              </div>
              <div className="sy-field">
                <label htmlFor="sy-budget-per-run-reserve">Per-run reserve (USD)</label>
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
                <label htmlFor="sy-budget-time-cost">Time cost per hour (USD)</label>
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
              <div className="sy-field">
                <label htmlFor="sy-budget-relay-chain-limit">Relay chain limit (USD)</label>
                <input
                  id="sy-budget-relay-chain-limit"
                  className="sy-field-input"
                  type="text"
                  inputMode="decimal"
                  title="What one chain of conversations handing work to each other may spend before the next handover is refused"
                  value={form.relayChainLimit}
                  onChange={(event) =>
                    setForm({ ...form, relayChainLimit: event.target.value })
                  }
                />
              </div>
            </div>
            <ConfirmButton
              label="Save budget"
              confirmLabel="Send these six fields to the daemon"
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
                  relay_chain_limit_usd: parseAmount(form.relayChainLimit),
                };
                setBudget.mutate(change, { onSuccess: () => { setBudgetSaved(true); } });
              }}
            />
            <p className="sy-meta" role="status">{budgetSaved ? "saved" : null}</p>
            {badCeilings.length > 0 && (
              <div id="sy-budget-error">
                <ErrorNote>
                  the budget was not sent — {badCeilings.join(" and ")} must be a
                  number, or blank for no ceiling
                </ErrorNote>
              </div>
            )}
            {setBudget.isError && (
              <ErrorNote>
                the budget was not changed — the {DAEMON_NAME} refused or is not responding
              </ErrorNote>
            )}
          </section>
        )}
        {quotaBrakeForm !== null && quotaBrake.data !== undefined && (
          <section className="sy-budget-group">
            <h3 className="sy-budget-heading">Quota brake</h3>
            <div className="sy-field">
              <label className="sy-check">
                <input
                  type="checkbox"
                  checked={quotaBrakeForm.enabled}
                  onChange={(event) => {
                    setQuotaBrakeForm({ ...quotaBrakeForm, enabled: event.target.checked });
                    setQuotaSaved(false);
                  }}
                />
                Enable quota brake
              </label>
            </div>
            <div className="sy-budget-fields">
              <div className="sy-field">
                <label htmlFor="sy-quota-brake-five-hour">Pause above 5h usage (%)</label>
                <input
                  id="sy-quota-brake-five-hour"
                  className="sy-field-input"
                  type="text"
                  inputMode="numeric"
                  value={quotaBrakeForm.fiveHour}
                  aria-invalid={badQuotaBrake ? true : undefined}
                  aria-describedby={badQuotaBrake ? "sy-quota-error" : undefined}
                  onChange={(event) => {
                    setQuotaBrakeForm({ ...quotaBrakeForm, fiveHour: event.target.value });
                    setQuotaSaved(false);
                    setBadQuotaBrake(false);
                  }}
                />
              </div>
              <div className="sy-field">
                <label htmlFor="sy-quota-brake-seven-day">Pause above 7d usage (%)</label>
                <input
                  id="sy-quota-brake-seven-day"
                  className="sy-field-input"
                  type="text"
                  inputMode="numeric"
                  value={quotaBrakeForm.sevenDay}
                  aria-invalid={badQuotaBrake ? true : undefined}
                  aria-describedby={badQuotaBrake ? "sy-quota-error" : undefined}
                  onChange={(event) => {
                    setQuotaBrakeForm({ ...quotaBrakeForm, sevenDay: event.target.value });
                    setQuotaSaved(false);
                    setBadQuotaBrake(false);
                  }}
                />
              </div>
            </div>
            <p className="sy-field-hint">
              When {quotaBrake.data.provider}'s usage passes either level, new autonomous work
              waits until the window resets. Only {quotaBrake.data.provider}'s windows count.
            </p>
            <ConfirmButton
              label="Save quota brake"
              confirmLabel="Save these three quota brake settings to the daemon"
              variant="ghost"
              onConfirm={() => {
                const fiveHour = parseQuotaPercent(quotaBrakeForm.fiveHour);
                const sevenDay = parseQuotaPercent(quotaBrakeForm.sevenDay);
                if (fiveHour === null || sevenDay === null) {
                  setBadQuotaBrake(true);
                  return;
                }
                setBadQuotaBrake(false);
                setQuotaBrake.mutate(
                  {
                    enabled: quotaBrakeForm.enabled,
                    pause_above_percent_5h: fiveHour,
                    pause_above_percent_7d: sevenDay,
                  },
                  { onSuccess: () => { setQuotaSaved(true); } },
                );
              }}
            />
            <p className="sy-meta" role="status">{quotaSaved ? "saved" : null}</p>
            {badQuotaBrake && (
              <div id="sy-quota-error">
                <ErrorNote>the quota brake was not sent — usage must be an integer from 1 to 100</ErrorNote>
              </div>
            )}
            {setQuotaBrake.isError && (
              <ErrorNote>the quota brake was not changed — the {DAEMON_NAME} refused or is not responding</ErrorNote>
            )}
          </section>
        )}
      </div>
    </Panel>
  );
}

/* ------------------------------------------------------------------ backups -- */

function BackupsView() {
  return (
    <div className="sy-backups-layout">
      <div className="sy-backups-main">
        <SpacePanel />
        <BackupsPanel />
      </div>
      <PiiObservations />
    </div>
  );
}

/**
 * What the database and the folders beside it occupy.
 *
 * A folder the daemon could not measure in time arrives as `null` and reads as an em dash with its
 * reason, never as zero. The reclaimable bar is the pages the file keeps after deletes. Giving them
 * back is a button, and only ever a button: nothing here suggests it. It schedules the compaction
 * for the next start, which is why the note beside it says that start takes longer.
 */
function SpacePanel() {
  const storage = useStorage();
  const stageCompaction = useStageCompaction();
  const space = storage.data;
  const unmeasured = "not measured in time";

  return (
    <Panel title="Space">
      {storage.isError && space === undefined && (
        <SystemListError error={storage.error} what="the space readout" />
      )}
      {!storage.isError && space === undefined && <Loading>reading the space used…</Loading>}
      {space !== undefined && (
        <>
          <div className="sy-space-cards">
            <StatCard
              label="Database"
              value={formatBytes(space.db_bytes)}
              detail={`${formatBytes(space.wal_bytes)} write-ahead log`}
              bar={
                <Meter
                  label="Reclaimable"
                  value={space.reclaimable_bytes}
                  ceiling={space.db_bytes}
                  tone="quantity"
                  format={formatBytes}
                />
              }
            />
            <StatCard
              label="Backups"
              value={space.backups_bytes === null ? undefined : formatBytes(space.backups_bytes)}
              unread={unmeasured}
            />
            <StatCard
              label="Files"
              value={space.files_bytes === null ? undefined : formatBytes(space.files_bytes)}
              unread={unmeasured}
            />
            <StatCard
              label="Logs"
              value={space.logs_bytes === null ? undefined : formatBytes(space.logs_bytes)}
              unread={unmeasured}
            />
          </div>
          <p className="sy-space-notes">
            {space.auto_backup_last === null ? (
              "No automatic backup has been taken yet."
            ) : (
              <>
                Last automatic backup <RelativeTime at={space.auto_backup_last} />.
              </>
            )}
            {space.loose_bytes > 0 && (
              <>
                {" "}
                {formatBytes(space.loose_bytes)} of old database copies lie beside the database.
              </>
            )}
          </p>
          {space.compaction_pending ? (
            <p className="sy-space-notes" role="status">
              Compaction is scheduled: the {DAEMON_NAME} rewrites the database when it next starts,
              and that start takes longer.
            </p>
          ) : (
            <div className="sy-space-compact">
              <ConfirmButton
                label="Compact on next start"
                confirmLabel="Compact when it next starts"
                sayAs={`Rewrite the database on the next start to give back ${formatBytes(space.reclaimable_bytes)}`}
                variant="ghost"
                disabled={stageCompaction.isPending}
                unavailable={space.reclaimable_bytes === 0}
                describedBy="space-compact-note"
                onConfirm={() => stageCompaction.mutate()}
              />
              <p className="sy-space-notes" id="space-compact-note">
                {space.reclaimable_bytes === 0 ? (
                  "Nothing to give back: the database has no free pages."
                ) : (
                  <>
                    Gives back about {formatBytes(space.reclaimable_bytes)}. The next start takes
                    longer while the file is rewritten. A safety copy of about{" "}
                    {formatBytes(Math.max(space.db_bytes - space.reclaimable_bytes, 0))} stays in
                    backups/ until the next compaction.
                  </>
                )}
              </p>
              {stageCompaction.isError && <CompactionError error={stageCompaction.error} />}
            </div>
          )}
        </>
      )}
    </Panel>
  );
}

function CompactionError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          conflict: "a restore is pending — compact after it has been applied",
        }}
      />
    );
  }
  return (
    <ErrorNote>
      the compaction could not be scheduled — the {DAEMON_NAME} is not responding
    </ErrorNote>
  );
}

/**
 * When a snapshot was taken, read back out of its name.
 *
 * `backup.rs` names every snapshot `[auto-]nucleos-<UTC stamp>-<seq>.db`, and that stamp is the only
 * time the listing carries. A name that does not fit the pattern answers `null` and the row
 * shows the name alone, rather than a date this page made up.
 */
export function takenAt(name: string): string | null {
  const match = /^(?:auto-)?nucleos-(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})/.exec(name);
  if (match === null) return null;
  const [, year, month, day, hour, minute, second] = match;
  return `${year}-${month}-${day}T${hour}:${minute}:${second}Z`;
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
  const noBackups = backups.data !== undefined && backups.data.length === 0;
  const latest = backups.data?.map((backup) => takenAt(backup.name)).find((at) => at !== null);
  const total = backups.data?.reduce((sum, backup) => sum + backup.size_bytes, 0) ?? 0;

  return (
    <Panel
      title="Backups"
      aside={
        <Button variant="ghost" disabled={takeBackup.isPending} onClick={() => takeBackup.mutate()}>
          Take a backup now
        </Button>
      }
    >
      {backups.data !== undefined && backups.data.length > 0 && (
        <p className="sy-backups-summary">
          {backups.data.length} {backups.data.length === 1 ? "snapshot" : "snapshots"} ·{" "}
          {formatBytes(total)}
          {latest !== undefined && (
            <>
              {" "}
              · latest <RelativeTime at={latest} />
            </>
          )}
        </p>
      )}
      <PanelNote empty={noBackups} says="no backup has been taken yet.">
        Staging a restore changes nothing yet — the swap happens the next time the {DAEMON_NAME} starts.
      </PanelNote>
      {takeBackup.isError && (
        <ErrorNote>
          the backup was not taken — the {DAEMON_NAME} refused or is not responding
        </ErrorNote>
      )}
      {backups.isError && backups.data === undefined && (
        <SystemListError error={backups.error} what="the backups" />
      )}
      {!backups.isError && backups.data === undefined && (
        <Loading>reading the backups…</Loading>
      )}
      {backups.data !== undefined && backups.data.length > 0 && (
        <Rows label="Backups">
          {backups.data.map((backup) => (
            <BackupRow key={backup.name} backup={backup} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

function BackupRow({ backup }: { backup: BackupInfo }) {
  const stageRestore = useStageRestore();
  const at = takenAt(backup.name);

  return (
    <Row dense className="sy-backup-row">
      <span className="sy-backup-when">
        {at === null ? "—" : <RelativeTime at={at} />}
      </span>
      {/* The file name, faint: it is what the daemon calls it, and the date in front of it
          is the same fact made readable. Clipped, with the whole of it as the tooltip. */}
      <span className="sy-backup-name" title={backup.name}>
        {backup.kind === "auto" && <span className="sy-backup-kind">automatic · </span>}
        {backup.name}
      </span>
      <span className="sy-backup-meta">{formatBytes(backup.size_bytes)}</span>
      <span className="sy-backup-meta">
        migration {backup.migration_version === null ? "unknown" : backup.migration_version}
      </span>
      <div className="sy-backup-action">
        <ConfirmButton
          label="Stage a restore"
          confirmLabel="Restore on next start"
          variant="ghost"
          disabled={stageRestore.isPending}
          onConfirm={() => stageRestore.mutate(backup.name)}
        />
      </div>
      {stageRestore.isSuccess && stageRestore.data !== undefined && (
        <p className="sy-restore-applied" role="status">
          Nothing has changed yet — {stageRestore.data.applies}
        </p>
      )}
      {stageRestore.isError && (
        <div className="sy-backup-error">
          <RestoreError error={stageRestore.error} />
        </div>
      )}
    </Row>
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
          unprocessable: `the ${DAEMON_NAME} could not verify that snapshot`,
          bad_request: `that snapshot name is not one the ${DAEMON_NAME} will accept`,
        }}
      />
    );
  }
  return (
    <ErrorNote>
      the restore could not be staged — the {DAEMON_NAME} is not responding
    </ErrorNote>
  );
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
      {!pii.isError && pii.data === undefined && <Loading>reading…</Loading>}
      {pii.data !== undefined && pii.data.length === 0 && <Quiet says="nothing recorded." />}
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

/** What each level allows, said beside the choice and not only in a paragraph above it. */
const TOKEN_LEVEL_HINT: Record<ApiTokenLevel, string> = {
  "read-only": "reads most things, not the budget or the kill switch",
  "run-creating": "can also start work",
  admin: "everything",
};

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
  // An admin token is everything, so the first press only asks; the second mints.
  const [adminAsked, setAdminAsked] = useState(false);

  function handleMint() {
    const trimmed = name.trim();
    if (trimmed === "") return;
    if (level === "admin" && !adminAsked) {
      setAdminAsked(true);
      return;
    }
    setAdminAsked(false);
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

      {/* One line: name, level, mint. Enter in the name box mints, the way a one-field form
          is expected to. */}
      <form
        className="sy-mint-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!mintToken.isPending) handleMint();
        }}
      >
        <div className="sy-field sy-mint-name">
          <label htmlFor="sy-token-name">Name</label>
          <input
            id="sy-token-name"
            className="sy-field-input"
            type="text"
            placeholder="e.g. ci-runner"
            maxLength={64}
            aria-describedby="sy-token-name-hint"
            value={name}
            onChange={(event) => {
              setName(event.target.value);
              setAdminAsked(false);
            }}
          />
          <span className="sy-field-hint" id="sy-token-name-hint">
            1&ndash;64 characters: letters, digits, hyphen or underscore.
          </span>
        </div>
        <fieldset className="sy-token-levels">
          <legend>Level</legend>
          <div className="sy-token-level-set">
            {TOKEN_LEVELS.map((candidate) => (
              <div key={candidate} className="sy-token-level-item">
              <label className="sy-token-level">
                <input
                  type="radio"
                  name="sy-token-level"
                  value={candidate}
                  checked={level === candidate}
                  aria-describedby={`sy-token-level-${candidate}`}
                  onChange={() => {
                    setLevel(candidate);
                    setAdminAsked(false);
                  }}
                />
                {candidate}
              </label>
              <span className="sr-only" id={`sy-token-level-${candidate}`}>
                {TOKEN_LEVEL_HINT[candidate]}
              </span>
              </div>
            ))}
          </div>
          <span className="sy-field-hint">{TOKEN_LEVEL_HINT[level]}</span>
        </fieldset>
        <Button
          type="submit"
          variant="ghost"
          disabled={mintToken.isPending || name.trim() === ""}
        >
          {adminAsked ? "Mint an admin token" : "Mint token"}
        </Button>
      </form>
      {adminAsked && (
        <p className="sy-note" role="status">
          An admin token can do everything this machine can, including the budget and the kill
          switch. Press the button again to mint it, or pick another level.
        </p>
      )}
      {mintToken.isError && <MintError error={mintToken.error} />}

      {tokens.isError && tokens.data === undefined && (
        <SystemListError error={tokens.error} what="the API tokens" />
      )}
      {!tokens.isError && tokens.data === undefined && (
        <Loading>reading the tokens…</Loading>
      )}
      {tokens.data !== undefined && tokens.data.length === 0 && (
        <Quiet says="no token has been minted." />
      )}
      {tokens.data !== undefined && tokens.data.length > 0 && (
        <Rows label="API tokens">
          {tokens.data.map((token) => (
            <TokenRow key={token.name} token={token} />
          ))}
        </Rows>
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
    <ErrorNote>the token was not minted — the {DAEMON_NAME} is not responding</ErrorNote>
  );
}

/** One row, with its own `useRevokeToken` instance — each row's pending/error state is its own. */
function TokenRow({ token }: { token: ApiTokenSummary }) {
  const revokeToken = useRevokeToken();

  return (
    <Row dense className="sy-token-row">
      <span className="sy-token-name" title={token.name}>
        {token.name}
      </span>
      <span>
        <Badge tone="info">{token.level}</Badge>
      </span>
      <span className="sy-token-meta">
        minted <RelativeTime at={token.created_at} />
      </span>
      <div className="sy-token-action">
        <ConfirmButton
          label="Revoke"
          /* The row names the token; the armed label only has to say the verb is final.
             With the name in it, every row's button was a different width and the
             columns before it could not line up. */
          confirmLabel="Really revoke"
          sayAs={`Revoke ${token.name}`}
          variant="danger"
          disabled={revokeToken.isPending}
          onConfirm={() => revokeToken.mutate(token.name)}
        />
      </div>
      {revokeToken.isError && (
        <div className="sy-token-error">
          <RevokeError error={revokeToken.error} />
        </div>
      )}
    </Row>
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
      that token was not revoked — the {DAEMON_NAME} is not responding
    </ErrorNote>
  );
}

/* ------------------------------------------------------------------- config -- */

/**
 * What the pillars are actually DOING, as the daemon has them in memory.
 *
 * These three stay here, and stay read-only, now that the Settings tab can
 * write the files they come from. They are not the same reading and neither
 * replaces the other: this is the parsed and running view — clamps applied,
 * `armed` computed, a malformed file already fallen back to defaults — where
 * the Settings tab shows the bytes on disk. The two disagree exactly when
 * somebody has edited a file and not restarted, and that gap is the thing
 * worth being able to see rather than the thing to design away.
 *
 * The panel that used to sit here naming `web`, `browser`, `council` and
 * `models` as areas with no configuration route is gone, because that is no
 * longer true of any of them.
 */
function ConfigIndex() {
  return (
    <div className="sy-config-grid">
      <EmailConfigPanel />
      <VoiceConfigPanel />
      <CalendarConfigPanel />
    </div>
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
        <Loading>reading…</Loading>
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
        <StateBadge domain="setting" state={config.enabled ? "enabled" : "disabled"} />
        <StateBadge domain="setting" state={config.armed ? "armed" : "unarmed"} />
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
          ? "local triage: no problem reported."
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
        <Loading>reading…</Loading>
      )}
      {voice.data !== undefined && <VoiceConfigFacts config={voice.data} />}
    </Panel>
  );
}

function VoiceConfigFacts({ config }: { config: VoiceConfig }) {
  return (
    <>
      <div className="sy-config-flags">
        <StateBadge domain="setting" state={config.armed ? "armed" : "unarmed"} />
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
        <Loading>reading…</Loading>
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

/* ------------------------------------------------------------------- shared -- */

/**
 * A panel's own prose — in front of a list that has something in it, one click
 * behind the line when it has not.
 *
 * The sentences are the same either way and what changes is where a reader
 * meets them. Above a populated list the note is what somebody needs *before*
 * pressing a button: that a brake holds what a project would start rather than
 * stopping what it is doing, that staging a restore changes nothing until the
 * núcleo restarts. Above an empty one it is a paragraph explaining rows that
 * are not there. Keeping it rather than cutting it is the point of the
 * disclosure — "no project is registered" on its own reads as a list that
 * failed to load, and the paragraph is what makes the emptiness a fact.
 *
 * Not every note belongs behind one. `TokensPanel`'s explains the mint form
 * above it and not the list below, and the moment the list is empty is exactly
 * when somebody is about to mint their first token and most needs the three
 * levels spelled out.
 */
/** "reading…" as a polite status, so the wait is announced and the layout has one name for it. */
function Loading({ children }: { children: ReactNode }) {
  return (
    <p className="sy-loading" role="status" aria-busy="true">
      {children}
    </p>
  );
}

function PanelNote({ empty, says, children }: { empty: boolean; says: string; children: ReactNode }) {
  if (empty) return <Quiet says={says}>{children}</Quiet>;
  return <p className="sy-note">{children}</p>;
}

function SystemListError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return (
    <ErrorNote>
      the {DAEMON_NAME} is not responding — nothing is known about {what}
    </ErrorNote>
  );
}
