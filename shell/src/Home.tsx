import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  getBudget, getProjects, getProposals,
  type Budget, type ConnectionState, type ProjectSummary, type Proposal,
} from "./api";
import { autopilotState, formatUsd, periodLabel, totalPending } from "./derive";
import { Button, Teach } from "./ui";

interface HomeProps {
  token: string | null;
  connection: ConnectionState;
  status: string | null;
  killEngaged: boolean | null;
  onOpenAutopilot: () => void;
}

function Stat({ n, k, sub, attn = false }: { n: ReactNode; k: string; sub?: ReactNode; attn?: boolean }) {
  return (
    <div className={attn ? "stat attn" : "stat"}>
      <span className="n">{n}</span>
      <span className="k">{k}</span>
      {sub !== undefined && <span className="sub">{sub}</span>}
    </div>
  );
}

function Home({ token, connection, status, killEngaged, onOpenAutopilot }: HomeProps) {
  const [projects, setProjects] = useState<ProjectSummary[] | null>(null);
  const [proposals, setProposals] = useState<Proposal[] | null>(null);
  const [budget, setBudget] = useState<Budget | null>(null);
  const inFlight = useRef(false);

  // Read-only digest on the same 3s cadence as the health poll. Home and
  // Autopilot are never mounted together (tabs render one or the other), so this
  // never double-polls; keeping prior values across refetches avoids flicker.
  useEffect(() => {
    if (token === null || connection !== "connected") {
      setProjects(null);
      setProposals(null);
      setBudget(null);
      return;
    }
    let cancelled = false;
    const load = async () => {
      // One round at a time: a daemon slower than the 3s tick would otherwise
      // accumulate rounds whose answers land out of order, so the digest could
      // settle on older numbers than it had already shown.
      if (inFlight.current) return;
      inFlight.current = true;
      try {
        const [nextProjects, nextProposals, nextBudget] = await Promise.all([
          getProjects(token),
          getProposals(token),
          getBudget(token),
        ]);
        if (cancelled) return;
        setProjects(nextProjects);
        setProposals(nextProposals);
        setBudget(nextBudget);
      } finally {
        inFlight.current = false;
      }
    };
    void load();
    const id = setInterval(() => void load(), 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [token, connection]);

  const loaded = projects !== null;
  const pending = totalPending(projects ?? []);
  const proposalCount = proposals?.length ?? 0;
  const activeCount = (projects ?? []).filter((project) => project.mode === "active").length;
  const shadowCount = (projects ?? []).filter((project) => project.mode === "shadow").length;
  const state = autopilotState({
    killEngaged,
    budgetPaused: budget?.paused === true,
    isFirstProject: projects?.length === 0,
    proposalCount,
    pending,
  });
  const attention = state === "kill" || state === "budget" || state === "swamped" || state === "pending";

  const headline = !loaded ? (
    connection === "connected" && status
      ? <>The núcleo is live. <span className="ok">Gathering your picture…</span></>
      : <>The núcleo is checking in.</>
  ) : state === "kill" ? <><span className="bad">Everything is stopped.</span> The kill switch is engaged.</>
    : state === "budget" ? <><em>Paused by budget</em> — approvals still work.</>
    : state === "first" ? <>No projects under autopilot yet.</>
    : state === "swamped" ? <><em>{proposalCount} decisions</em> are waiting to clear.</>
    : state === "pending" ? <>All quiet — <em>{pending} decisions</em> waiting on you.</>
    : <>All quiet. <span className="ok">The núcleo is holding steady.</span></>;

  const ctaLabel = state === "pending" ? `Review ${pending} ${pending === 1 ? "decision" : "decisions"}`
    : state === "swamped" ? "Clear the approval queue"
    : state === "kill" ? "Open Autopilot to disengage"
    : state === "first" ? "Bring a project aboard"
    : "Open Autopilot";

  return (
    <section className="home-digest">
      <h1 className="headline">{headline}</h1>
      <div className="statusline">
        <span data-testid="connection-state">connection <b>{connection}</b></span>
        <span data-testid="daemon-status">daemon <b>{status ?? "waiting for status"}</b></span>
      </div>
      {!loaded ? (
        <Teach title="Your operating picture is loading.">
          Once Autopilot reports projects and activity, the quiet summary lands right here.
        </Teach>
      ) : (
        <>
          <div className="digest-stats">
            <Stat n={projects.length} k="projects" sub={<>{activeCount} active · {shadowCount} shadow</>} />
            <Stat n={pending} k="awaiting review" sub="shadow decisions" attn={pending > 0} />
            <Stat n={proposalCount} k="approval queue" sub={proposalCount === 1 ? "proposal" : "proposals"} attn={proposalCount > 0} />
            <Stat
              n={budget === null ? "—" : formatUsd(budget.window_spend_usd)}
              k="spend"
              sub={budget === null ? undefined : <>{periodLabel(budget.period)} · {budget.limit_usd === null ? "no limit" : `cap ${formatUsd(budget.limit_usd)}`}</>}
              attn={budget?.paused === true}
            />
          </div>
          <div className="digest-cta">
            <Button intent={attention ? "go" : undefined} onClick={onOpenAutopilot}>{ctaLabel}</Button>
            {state === "quiet" && <span className="cta-note">Nothing needs your signature right now.</span>}
          </div>
        </>
      )}
    </section>
  );
}

export default Home;
