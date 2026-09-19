import { useMemo } from "react";
import { Outlet, useNavigate } from "@tanstack/react-router";
import { useChats } from "../data/chats";
import { untriagedCount, useMailQueue } from "../data/mail";
import { POLL } from "../data/poll";
import { useProjects, useSystemHealth, wantsAttention } from "../data/system";
import { useWaitingCount } from "../data/waiting";
import { unreadTotal } from "../lib/turns";
import { PaletteProvider, usePaletteGroup, type PaletteGroup } from "../ui";
import { AttentionHeartbeat } from "./AttentionHeartbeat";
import { ConnectionGate } from "./ConnectionGate";
import { KillSwitchControl } from "./KillSwitchControl";
import { NotificationsDrawer } from "./NotificationsDrawer";
import { PaletteTrigger } from "./PaletteTrigger";
import { QuotaNotch } from "./QuotaNotch";
import { useNotchMode, useSetNotchMode } from "./notch-mode";
import { Sidebar } from "./Sidebar";
import { NAV_ITEMS } from "./nav";

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
  /**
   * Whether anything in the machine wants looking at, for the rail's one dot.
   *
   * This used to read `useHealth()` — the gate's own liveness check — and ask
   * whether it was `false`. It could not be: `ConnectionGate` wraps this whole
   * component and lets it render only when that query has answered `true`, so
   * the expression was provably false on every frame the rail has ever drawn.
   * The dot was machinery that could not fire, tested and all.
   *
   * The subsystem readout is the reading that CAN say something: it is the
   * daemon's own aggregate over sqlite, the CLI binary, the credential manager,
   * the worktree disk, five sidecars and the transcriber. Which of its four
   * statuses counts as trouble is `wantsAttention`'s to say, in the file that
   * owns the type, where a test holds it.
   *
   * Asked at the slow cadence, since the rail only needs to know whether to point
   * at System — and the System page itself asks fast while somebody is reading
   * it, off the same cache entry.
   */
  const health = useSystemHealth(POLL.slow);
  /** The six decision lists share one count, so the rail cannot drift from Waiting. */
  const waiting = useWaitingCount();
  /**
   * Unread turns, summed across every conversation. `undefined` until the
   * list has answered once — the same honest-absence rule as `waiting`
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
    <PaletteProvider>
      <div className="app-shell">
        <AttentionHeartbeat />
        <Destinations />
        <Sidebar
          badges={{
            proposals: waiting,
            chats: chats.data === undefined ? undefined : unreadTotal(chats.data),
            mail: mail.data === undefined ? undefined : untriagedCount(mail.data),
          }}
          projects={projects.data?.map((project) => ({
            id: project.project_id,
            mode: project.mode,
            pending: project.open_review_items,
          }))}
          systemAlert={wantsAttention(health.data)}
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
          {/*
            First into the slot, and put here rather than in `Sidebar.tsx`: the
            slot's own comment says that only whoever fills it knows where the
            break falls, and the rail does not otherwise know a palette exists.
            `Sidebar.tsx` stays untouched, and so does its twenty-five-case test.
          */}
          <PaletteTrigger />
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
          {/*
            The notch, contained. At the top edge of the page area rather than inside it, because it
            is about the machine and not about whatever page is open — and because that is where
            the floating window hangs too, so the drawing does not move between the two hosts.
          */}
          <ContainedNotch />
          <div className="app-page">
            <Outlet />
          </div>
        </main>
      </div>
    </PaletteProvider>
  );
}

/**
 * The quota notch, when the owner keeps it inside the app (design D8).
 *
 * Drawn only once the mode has answered `contained`. Before that answer the right picture is
 * nothing, not a guess: with the notch floating, a contained one drawn "until we know" would show
 * the same reading twice on every launch.
 */
function ContainedNotch() {
  const mode = useNotchMode();
  const setMode = useSetNotchMode();
  if (mode !== "contained") return null;
  return <QuotaNotch host="contained" onMove={() => void setMode("global").catch(() => {})} />;
}

/**
 * The nineteen places the rail goes, contributed to the palette exactly the way
 * a page contributes its own rows.
 *
 * It renders nothing — the shape `AttentionHeartbeat` already uses — and it
 * lives here rather than inside `ui/Palette.tsx` because the primitive imports
 * nothing from `app/`. There is one registration mechanism and the app is
 * merely its first caller; a palette that knew about the nav table would be a
 * second one, and the two would drift.
 */
function Destinations() {
  const navigate = useNavigate();
  const group = useMemo<PaletteGroup>(
    () => ({
      id: "go",
      heading: "Go to",
      items: NAV_ITEMS.map((item) => ({
        id: item.id,
        label: item.label,
        /* The label, and nothing else the rail holds: a path is not a word
           anybody types, and `nav.ts` carries the only human-readable name this
           shell has for a route — `router.tsx` has no titles. */
        match: item.label,
        /* Listed, disabled, with the reason as the hint. `nav.ts` already rules
           that hiding such an item "would be the shell pretending the feature
           was never designed", and the palette does not get to overturn the
           rail on the rail's own list. */
        hint: item.disabled,
        disabled: item.disabled !== undefined,
        run: () => void navigate({ to: item.path }),
      })),
    }),
    [navigate],
  );

  usePaletteGroup(group);
  return null;
}
