import { Outlet } from "@tanstack/react-router";
import { useChats } from "../data/chats";
import { untriagedCount, useMailQueue } from "../data/mail";
import { useHealth, useProjects, useProposals } from "../data/system";
import { unreadTotal } from "../lib/turns";
import { AttentionHeartbeat } from "./AttentionHeartbeat";
import { ConnectionGate } from "./ConnectionGate";
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
   *
   * Handed over on every page even though the rail only draws it inside the
   * projects area, and that is not waste: the query is shared, so the request
   * happens either way, and deciding *where* the roster is shown is the rail's
   * business rather than something this component should have to know.
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
        {/*
          The connection line used to be here, and it was removed on 2026-09-05
          because it could only ever say one thing.

          `ConnectionGate` wraps this whole component and `read()` returns
          "through" only when `health.data === true`; every other reading —
          connecting, unreachable, unauthorised — replaces the entire window with
          a takeover. So by the time the rail is on screen the daemon is
          answering by construction, and a status line inside it was a green dot
          that had no second state to show. The two states worth seeing are shown
          where they take over the screen, which is where somebody can act on
          them.
        */}
        {/*
          Two destinations, then the stop, and nothing else.

          The budget line was the third thing here and came out on 2026-09-05 on
          the owner's call. Unlike the connection line above it, it was not dead —
          it said something true — it was just not worth a permanent row: the
          figure is already on Home, Fleet, Autopilot, System and a project's
          Estado mode, all of which are places somebody goes to think about
          spending. A rail is for getting somewhere and for stopping the machine.

          The drawer is above the kill switch and never below it. The switch is
          the one control that must be reachable without aiming, from every page,
          and inserting anything under it would move it off the bottom edge
          people already know — the drawer is somewhere you choose to go, which
          is a lower claim on the footer than the emergency stop has.
        */}
        <NotificationsDrawer />
        {/*
          The break this footer actually has. Everything above it is somewhere to
          go or something to read; below it is the one control that stops the
          machine, and it gets the width and the distance that says so.
        */}
        <hr className="nav-rule" />
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
