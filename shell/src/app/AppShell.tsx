import { Outlet } from "@tanstack/react-router";
import { useChats } from "../data/chats";
import { untriagedCount, useMailQueue } from "../data/mail";
import { useHealth, useProjects, useProposals } from "../data/system";
import { unreadTotal } from "../lib/turns";
import { AttentionHeartbeat } from "./AttentionHeartbeat";
import { BudgetLine } from "./BudgetLine";
import { ConnectionGate } from "./ConnectionGate";
import { ConnectionStatus } from "./ConnectionStatus";
import { KillSwitchControl } from "./KillSwitchControl";
import { NotificationsDrawer } from "./NotificationsDrawer";
import { Sidebar } from "./Sidebar";

/**
 * Everything that is true on every page.
 *
 * The rail on the left, the page on the right, the handshake in front of both,
 * and the heartbeat that tells the núcleo whether anyone is watching.
 */
export function AppShell() {
  return (
    <ConnectionGate>
      <Frame />
    </ConnectionGate>
  );
}

/**
 * The shell proper, and the reason it is a separate component: its queries must
 * not start until the gate has let us through.
 *
 * Written the other way round — hooks in `AppShell`, gate in its return — the
 * roster and the badge counts would poll every three seconds *behind* a
 * takeover that exists precisely because the daemon is not answering, filling
 * the log with failures nobody can act on.
 */
function Frame() {
  const health = useHealth();
  /**
   * `GET /proposals` is `list_pending` in the núcleo — the route serves the
   * queue, not the archive — so its length *is* the badge. Filtering by status
   * here would be the shell second-guessing a decision the route already made.
   */
  const proposals = useProposals();
  /**
   * Unread turns, summed across every conversation. `undefined` until the
   * list has answered once — the same honest-absence rule as `proposals`
   * above, read through `unreadTotal` rather than a bare `.length` because a
   * chat's badge is `waiting`, not a row count.
   */
  const chats = useChats();
  /**
   * Untriaged mail. `undefined` until the queue has answered once, same rule
   * as the two above — a zero drawn before the first answer would be a claim
   * nobody has measured, not an honest "nothing waiting".
   */
  const mail = useMailQueue();
  /**
   * The roster, for the sidebar's project group.
   *
   * Already polled — this is the same query the roster page and the pending
   * counts read, so the rail costs no extra request. `undefined` until it has
   * answered once, and the rail draws no rows for that rather than an empty
   * group: a heading with nothing under it reads as "you have no projects",
   * which is a claim about the daemon's answer before it gave one.
   */
  const projects = useProjects();

  return (
    <div className="app-shell">
      <AttentionHeartbeat />
      <Sidebar
        badges={{
          proposals: proposals.data?.length,
          chats: chats.data === undefined ? undefined : unreadTotal(chats.data),
          mail: mail.data === undefined ? undefined : untriagedCount(mail.data),
        }}
        projects={projects.data?.map((project) => ({
          id: project.project_id,
          mode: project.mode,
          pending: project.open_proposals,
        }))}
        systemAlert={health.data === false}
      >
        <ConnectionStatus />
        <BudgetLine />
        {/*
          Above the kill switch and never below it. The switch is the one control
          that must be reachable without aiming, from every page, and inserting
          anything under it would move it off the bottom edge people already know
          — the drawer is somewhere you choose to go, which is a lower claim on
          the footer than the emergency stop has.
        */}
        <NotificationsDrawer />
        <KillSwitchControl />
      </Sidebar>
      <main className="app-main">
        <div className="app-page">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
