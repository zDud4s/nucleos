import { Outlet } from "@tanstack/react-router";
import { useHealth, useProposals } from "../data/system";
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
   *
   * The other two badge sources, unread chats and untriaged mail, have no hook
   * yet: they arrive with the slices that build those pillars. Until then those
   * items simply carry no badge, which is the honest rendering of a count
   * nobody has measured — a zero would be a claim.
   */
  const proposals = useProposals();

  return (
    <div className="app-shell">
      <AttentionHeartbeat />
      <Sidebar badges={{ proposals: proposals.data?.length }} systemAlert={health.data === false}>
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
