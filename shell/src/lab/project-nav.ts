/**
 * This app's rail, in the shape the pasted component eats.
 *
 * The whole point of the lab: the question is not "does the registry's demo
 * render", it is "does OUR nav survive this component". So the table below is
 * not retyped — it is `src/app/nav.ts` read at import time and mapped. If
 * somebody adds a page to the app tomorrow, the lab grows a row without anyone
 * touching this file, and if somebody deletes one it disappears. A hand-copied
 * list would have been right on the day it was written and quietly wrong after.
 *
 * Two things the mapping has to invent, because `nav.ts` deliberately does not
 * carry them:
 *
 * **Icons.** `nav.ts` spells a two-letter `glyph` per item and says why: the
 * CSP forbids remote assets, glyphs read the same at any zoom, and four items
 * start with C. The pasted component wants a `React.ElementType`, so the lab
 * assigns a lucide icon per id in {@link ICONS}. That is a decision about the
 * *candidate design*, not about the app, which is exactly why it lives here and
 * not in `nav.ts`. Adopting the component means either moving this table into
 * `nav.ts` or teaching the component to draw a monogram — a real choice, and
 * seeing both rails side by side is how it gets made.
 *
 * **Roster rows.** `nav.ts` marks the projects group `roster: true`: its rows
 * come from the daemon, not from the compiled list. The pasted component has
 * exactly one nesting mechanism, `children`, so the roster maps onto it — which
 * is the happiest surprise of the exercise, since the app's own rail draws that
 * group flat and conditionally. Fixture names, matched to `src/preview/daemon.ts`,
 * so the lab and the screenshot harness are describing the same núcleo.
 */
import {
  Activity,
  Bot,
  Boxes,
  Calendar,
  Compass,
  Contact,
  Files,
  FolderKanban,
  Gauge,
  GraduationCap,
  Globe,
  Hash,
  Hourglass,
  LayoutDashboard,
  ListChecks,
  Mail,
  MessagesSquare,
  Mic,
  Scale,
  Search,
  SlidersHorizontal,
  Rss,
  Users,
} from "lucide-react";
import { NAV, SYSTEM_ITEM, type NavItem } from "../app/nav";
import type { NavGroupData, NavItemData, WorkspaceOption } from "./dashboard-sidebar";

/**
 * One lucide icon per nav id.
 *
 * Keyed by `NavItem["id"]` rather than by label, for the reason `nav.ts` gives
 * for the badge names: the id is the stable thing, the label is what somebody
 * renames on a Tuesday. A `Record` and not a `Map` so a missing id is a
 * compile-time hole once the union is tightened; today it falls back to `Hash`,
 * which shows up on screen as an obviously-wrong row rather than as a crash.
 */
const ICONS: Record<string, NavItemData["icon"]> = {
  /* Operate — the state of the machine right now. */
  home: LayoutDashboard,
  fleet: Boxes,
  autopilot: Gauge,
  waiting: Hourglass,
  runs: Activity,
  feed: Rss,
  learned: GraduationCap,

  /* Projects. */
  projects: FolderKanban,

  /* Work — what you and the agents are doing together. */
  chats: MessagesSquare,
  errands: ListChecks,
  teams: Users,
  agents: Bot,
  council: Scale,

  /* Pillars — the outside world the núcleo reaches into. */
  mail: Mail,
  contacts: Contact,
  calendar: Calendar,
  voice: Mic,
  web: Globe,
  browser: Compass,
  files: Files,

  /* Under the rule. */
  system: SlidersHorizontal,
};

/**
 * The projects the daemon would name, as the roster group's children.
 *
 * Four, and the same four `src/preview/daemon.ts` serves, so a shot of the lab
 * and a shot of the app are talking about the same fixtures.
 */
const ROSTER = ["alpha", "bravo", "charlie", "delta"];

/**
 * What a badge would say.
 *
 * `nav.ts` declares a badge as a NAME — `proposals`, `chats`, `mail` — and
 * leaves the counting to the AppShell, because the sidebar does not know how to
 * count unread mail. The lab is not wired to the núcleo, so it stands in a
 * plausible number and the seam stays visible: these are the three rows that
 * have to be fed from outside when the component is adopted.
 */
const BADGES: Record<string, number> = { proposals: 3, chats: 7, mail: 12 };

function toItem(item: NavItem): NavItemData {
  return {
    id: item.id,
    title: item.label,
    icon: ICONS[item.id] ?? Hash,
    badge: item.badge === undefined ? undefined : BADGES[item.badge],
  };
}

/**
 * `NAV`, group for group, plus the search row the rail does not have.
 *
 * Search is prepended rather than dropped: the app really does have a ⌘K
 * palette (`cmdk`, per project and in Chats), it is just reached from inside a
 * page rather than from the rail. The pasted design puts it at the top, and
 * whether that is an improvement is one of the things the lab exists to show —
 * so it is drawn, and labelled honestly as not-currently-in-the-rail.
 */
export const PROJECT_NAV_GROUPS: NavGroupData[] = [
  {
    items: [{ id: "search", title: "Search", icon: Search, shortcut: "⌘K" }],
  },
  ...NAV.map((group): NavGroupData => {
    const items = group.items.map(toItem);

    /*
      The roster group's one compiled item — `All projects` — keeps its place and
      grows the daemon's rows underneath it. `children` is the component's only
      nesting mechanism and the roster is the app's only nested thing, so this is
      the mapping rather than a mapping.
    */
    if (group.roster === true && items[0] !== undefined) {
      items[0] = {
        ...items[0],
        children: ROSTER.map((id) => ({ id: `project-${id}`, title: id, icon: Hash })),
      };
    }

    return { heading: group.label, items };
  }),
];

/** System, where the pasted design keeps Settings and Log out. */
export const PROJECT_NAV_BOTTOM: NavItemData[] = [{ ...toItem(SYSTEM_ITEM), shortcut: "⌘," }];

/**
 * The switcher under the mark — projects, with the núcleo itself at the top.
 *
 * The paste's `WorkspaceSwitcher` is a tenant picker: `Acme Corp`, `Personal
 * Workspace`, `Create Workspace`, `Pro Plan`. None of that exists here. NucleOS
 * is a local desktop núcleo with no tenants and no plans, and the real rail's
 * `.nav-brand` holds only a wordmark and the collapse button. An earlier draft
 * of this file filled the slot with three invented names, which is worse than
 * leaving it empty: it made the component look integrated while showing data
 * that corresponds to nothing.
 *
 * Projects are what belongs there, and the reason is in `nav.ts`. The Projects
 * group was promoted from an item precisely because reaching a project cost "a
 * list to open and then a choice, on every single entry" — and then the roster
 * rows were made **conditional**, drawn only while you are already in the
 * projects area. That correction was right for the rail's length and it gave
 * the cost back: from the Feed, a project is once again a list and a choice.
 * A switcher pinned above the scroll is where that access can live without the
 * roster's length pushing Work and Pillars off the bottom.
 *
 * `NucleOS` first and separated, because it is not a project: it is the way
 * back out to Home. The separator is the component's own `h-px` rule, so the
 * break costs no new markup.
 *
 * The notes are each project's autopilot mode, which is a real field on
 * `ProjectSummary` and the one fact about a project worth reading before you
 * switch into it. Matched to `src/preview/daemon.ts` so the lab and the
 * screenshot harness describe the same núcleo.
 */
export const PROJECT_WORKSPACES: WorkspaceOption[] = [
  /*
    `leading` and not just `separatorAfter`. A rule between two groups says
    "these are different lists"; it does not survive the case the owner named —
    a project called `nucleos` sitting one line under `NucleOS`, same word, same
    weight, distinguishable only by which side of a hairline it fell on. The
    mark against a mode dot is a difference of KIND, and no project name can
    collide with it. Filled in by `main.tsx`, which is where the asset is.
  */
  { label: "NucleOS", note: "the núcleo", separatorAfter: true },
  { label: "alpha", note: "shadow", mode: "shadow" },
  { label: "bravo", note: "active", mode: "active" },
  { label: "charlie", note: "off", mode: "off" },
  { label: "delta", note: "shadow", mode: "shadow" },
];

/**
 * What the menu's last row offers.
 *
 * `New project` and not `Create Workspace`, and it is not a rename for taste:
 * `/projects/new` is a route this app actually has (`router.tsx`, and
 * `NewProject.tsx` behind it), so the row can be wired to something. The paste's
 * label could not be.
 */
export const PROJECT_CREATE_LABEL = "New project";

/**
 * The mark, and the one place in the rail it is allowed to appear.
 *
 * `tokens.css` is unusually explicit about this and the owner confirmed it:
 * `--accent` (#4fd1e0 dark, #0b8493 light) is "the one brand colour. Not a
 * state; never used to mean anything", reserved for the wordmark, links and the
 * focus ring. So cyan in this component means *NucleOS*, and it may not mean
 * "three things are waiting" — which is why the badges stay on `--solid` and are
 * left exactly as the paste drew them.
 *
 * Imported rather than referenced by URL so the hash lands in the build and the
 * asset cannot go missing silently. `src/assets/brand/` and not `public/brand/`
 * for the same reason: `public/` is copied verbatim and never checked.
 */
export { default as PROJECT_MARK } from "../assets/brand/mark.png";
export { default as PROJECT_LOCKUP } from "../assets/brand/lockup.png";
