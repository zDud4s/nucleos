import { Link, useParams } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  isAggregateTimeout,
  useSidecars,
  useSystemHealth,
  type HealthReadout,
  type SidecarState,
  type SubsystemReadout,
} from "../data/system";
import { ErrorNote, PageHeader, Panel, RefusalNote, RelativeTime, StateBadge, Teach } from "../ui";
import "./system.css";

/**
 * System — the machine's own state, not a project's.
 *
 * Three tabs. This slice builds the health view: the daemon's own subsystem
 * readout and the sidecars' own liveness, both read off `data/system.ts`'s
 * `useSystemHealth`/`useSidecars` — the ONE health query in the app, which
 * every pillar's own health reading (Browser's included) narrows rather than
 * asking again.
 *
 * Backups and tokens arrive with the packets that build them (S2/S3). This
 * page says so plainly rather than drawing an empty panel that looks broken.
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
        {view === "backups" && <ArrivesLater view="backups" />}
        {view === "tokens" && <ArrivesLater view="tokens" />}
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
function ArrivesLater({ view }: { view: "backups" | "tokens" }) {
  const subject = view === "backups" ? "the backup ledger and its own controls" : "token spend and secret access";
  return (
    <Teach title={`${VIEW_LABEL[view]} arrives with the next packet`}>
      <p>This tab is real and will stay — only its content is still to come: {subject}.</p>
    </Teach>
  );
}
