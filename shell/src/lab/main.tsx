import { useEffect, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import "../fonts.css";
import "../tokens.css";
import "../tailwind.css";
import "../base.css";
import "../ui.css";
import "./prompt-design.css";
import "./lab.css";
import { adoptStyleNonce } from "../lib/style-nonce";
import SidebarNavPreview, { mockBottomItems, mockNavGroups } from "./dashboard-sidebar";
import {
  PROJECT_CREATE_LABEL,
  PROJECT_LOCKUP,
  PROJECT_MARK,
  PROJECT_NAV_BOTTOM,
  PROJECT_NAV_GROUPS,
  PROJECT_WORKSPACES,
} from "./project-nav";

/**
 * The bench for `dashboard-sidebar.tsx`, before it is anywhere near the app.
 *
 * **A fourth entry, and never part of the app.** Nothing under `src/` imports
 * this file; the only thing that reaches it is `sidebar-lab.html`, which the
 * app's own build never has as an input. Same discipline as `src/preview/`,
 * stated in `preview.html`: `dist/` is embedded into the Tauri binary at compile
 * time and must hold the app and nothing else.
 *
 * # What it answers
 *
 * Two questions that a paste cannot answer on its own, and that a screenshot
 * from the registry answers wrongly:
 *
 * **Does it hold OUR nav?** The registry's demo has four groups, two levels and
 * fourteen rows chosen to flatter the layout. This app's rail has four groups,
 * twenty-one rows, three badges, and a roster group whose length belongs to the
 * daemon. The `Tabs` control swaps between them; the project side is read out of
 * `src/app/nav.ts` at import, so it cannot drift from what the app actually has.
 *
 * **What does it look like in THIS document?** `tailwind.css` clears the colour,
 * font, shadow and radius namespaces before refilling them from `tokens.css`,
 * which is a deliberate tripwire — a registry component's decoration is supposed
 * to compile to nothing so it shows up in review. The consequence is that the
 * right-hand panel is the honest answer to "what happens if I drop this in
 * today", and it is NOT a fair picture of the design. So the left-hand panel
 * fills the holes back in (`prompt-design.css`) and shows what the author drew.
 * The gap between the two panels is the adoption cost, itemised.
 *
 * **Does every part of it have a job here?** Not by default, and the switcher
 * under the mark is where that shows. The paste's is a tenant picker for a
 * multi-tenant SaaS; this app has no tenants. `project-nav.ts` re-points it at
 * projects with the núcleo itself as the way back to Home — which is a design
 * decision the owner made, not one the paste implied, and the reasoning is
 * recorded beside the table there.
 *
 * # The five stylesheets, in the app's order
 *
 * `fonts`, `tokens`, `tailwind`, `base`, `ui` — the same imports as `main.tsx`
 * and `preview/main.tsx`, in the same order, because the cascade is fixed by
 * import order and a bench that loaded a different document floor would be
 * measuring a different page. `app.css` is the one the app has that this does
 * not: it styles the real shell, which is not on screen here.
 *
 * # Themes
 *
 * There is no theme toggle, on purpose. `tokens.css` switches on
 * `prefers-color-scheme` and nothing else — no class, no attribute — so the only
 * truthful way to see the light theme is to ask the browser for it, exactly as
 * `scripts/preview-shots.mjs` does with Playwright's `features: [{ name:
 * "prefers-color-scheme", value: "light" }]`. In a browser: DevTools →
 * Rendering → Emulate CSS media feature prefers-color-scheme. A button here
 * would have meant restating the light palette in a third place, and a third
 * copy of a palette is how two of them start to drift.
 */

adoptStyleNonce();

type Tabs = "project" | "registry";
type View = "compare" | "prompt" | "tokens";

function Segments<T extends string>({
  label,
  value,
  onChange,
  options,
}: {
  label: string;
  value: T;
  onChange: (next: T) => void;
  options: { id: T; label: string }[];
}) {
  return (
    <div className="lab-control">
      <span>{label}</span>
      <div className="lab-segments">
        {options.map((option) => (
          <button
            key={option.id}
            type="button"
            className="lab-segment"
            aria-pressed={option.id === value}
            onClick={() => onChange(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * What the rail is hiding, measured off the DOM rather than argued about.
 *
 * The single most useful thing this lab found, and the one nobody would have
 * found by reading the paste: the scroll container is
 * `overflow-y-auto [&::-webkit-scrollbar]:hidden [scrollbar-width:none]
 * [-ms-overflow-style:none]`, which suppresses the scrollbar on all three
 * engines. With the registry's fourteen rows that is a tidy choice and costs
 * nothing. With this app's rail it silently swallows the end of the list — and
 * because the bar is gone there is no scrollbar, no fade and no arrow to say
 * so. It reads as "those pages do not exist".
 *
 * Measured on every mount and on every resize, so the number on screen is this
 * window's, not one written down on the day the lab was built.
 */
function useClipped(frame: React.RefObject<HTMLDivElement | null>) {
  const [clip, setClip] = useState<{ px: number; hidden: string[] } | null>(null);

  useEffect(() => {
    const root = frame.current;
    if (root === null) return;

    const measure = () => {
      /*
        The rail's scroller is the first `overflow-y-auto` in the tree — the
        dashboard body beside it is the second. Queried by class because this is
        a bench looking at somebody else's markup, which is exactly the kind of
        coupling that would be wrong in the app and is fine here.
      */
      const scroller = root.querySelector(".overflow-y-auto");
      if (scroller === null) return;

      const px = scroller.scrollHeight - scroller.clientHeight;
      const box = scroller.getBoundingClientRect();
      const hidden = [...scroller.querySelectorAll("span.truncate")]
        .filter((row) => row.getBoundingClientRect().bottom > box.bottom + 1)
        .map((row) => (row.textContent ?? "").trim());

      setClip(px > 1 ? { px, hidden } : null);
    };

    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(root);
    return () => observer.disconnect();
  }, [frame]);

  return clip;
}

/**
 * One rendering of the paste, captioned with what it is a picture of.
 *
 * `prompt-design` on the wrapper is the whole of the difference between the two
 * panels — the component, its props and its class list are identical on both
 * sides. That is the claim the lab is making, and keeping the difference to a
 * single class name is what makes the claim checkable.
 */
function Panel({
  design,
  title,
  note,
  ...data
}: {
  design: "prompt" | "tokens";
  title: string;
  note: string;
  groups: typeof mockNavGroups;
  bottomItems: typeof mockBottomItems;
  workspaces: typeof PROJECT_WORKSPACES | string[];
  logo?: React.ReactNode;
  createLabel?: string;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const clip = useClipped(frame);

  return (
    <div className="lab-panel">
      <div className="lab-caption">
        <b>{title}</b>
        <span>{note}</span>
      </div>

      {clip !== null && (
        <p className="lab-clip">
          <b>{clip.px}px of the rail is unreachable</b> — and the scrollbar is suppressed, so
          nothing on screen says so.
          {clip.hidden.length > 0 && (
            <>
              {" "}
              Below the fold: <span>{clip.hidden.join(", ")}</span>.
            </>
          )}
        </p>
      )}

      <div ref={frame} className={`lab-frame ${design === "prompt" ? "prompt-design" : ""}`}>
        {/*
          Keyed by design AND by which nav is loaded, so switching either one
          remounts. The component keeps `activeId`, the open/closed of every
          expandable row and the workspace in its own `useState`; without a key,
          swapping the nav table under it leaves it pointing at an id the new
          table does not have, and the breadcrumb reads "Dashboard" for reasons
          that have nothing to do with the design.
        */}
        <SidebarNavPreview
          key={`${design}-${data.groups.length}-${data.workspaces.length}`}
          groups={data.groups}
          bottomItems={data.bottomItems}
          workspaces={data.workspaces}
          plan={design === "prompt" ? "Pro Plan" : undefined}
          logo={data.logo}
          createLabel={data.createLabel}
        />
      </div>
    </div>
  );
}

function Lab() {
  const [tabs, setTabs] = useState<Tabs>("project");
  const [view, setView] = useState<View>("compare");

  /*
    The mark, dropped into the núcleo's row where a project would carry its mode
    dot. Built here and not in `project-nav.ts` because that file is data and
    this one is the only place that knows how the asset is drawn.
  */
  const switcher = PROJECT_WORKSPACES.map((row) =>
    row.label === "NucleOS"
      ? { ...row, leading: <img className="lab-mark-mini" src={PROJECT_MARK} alt="" /> }
      : row,
  );

  const data =
    tabs === "project"
      ? {
          groups: PROJECT_NAV_GROUPS,
          bottomItems: PROJECT_NAV_BOTTOM,
          workspaces: switcher,
          /*
            The mark rides with the DATA, not with the design toggle — so it
            shows in both panels for this project and in neither for `Acme
            Corp`. A logo belongs to a workspace; the registry's demo has no
            NucleOS in it, and giving one to Acme would be dressing somebody
            else's screenshot in our brand.
          */
          logo: <img className="lab-mark" src={PROJECT_MARK} alt="" />,
          createLabel: PROJECT_CREATE_LABEL,
        }
      : {
          groups: mockNavGroups,
          bottomItems: mockBottomItems,
          workspaces: ["Acme Corp", "Personal Workspace", "Client Sandbox"],
          logo: undefined,
          createLabel: undefined,
        };

  const showPrompt = view !== "tokens";
  const showTokens = view !== "prompt";

  return (
    <div className="lab">
      <header className="lab-header">
        <div className="lab-title">
          <img className="lab-lockup" src={PROJECT_LOCKUP} alt="NucleOS" />
          <h1>dashboard-sidebar.tsx — lab</h1>
          <p>
            The paste, unmodified, rendered twice. Left: what it was drawn against. Right: what this
            app's document actually gives it.
          </p>
        </div>

        <Segments<Tabs>
          label="Tabs"
          value={tabs}
          onChange={setTabs}
          options={[
            { id: "project", label: "This project" },
            { id: "registry", label: "Registry demo" },
          ]}
        />

        <Segments<View>
          label="View"
          value={view}
          onChange={setView}
          options={[
            { id: "compare", label: "Compare" },
            { id: "prompt", label: "Prompt design" },
            { id: "tokens", label: "Project tokens" },
          ]}
        />
      </header>

      <div className="lab-panels" data-view={view === "compare" ? "compare" : "solo"}>
        {showPrompt && (
          <Panel
            design="prompt"
            title="Prompt design"
            note="shadcn neutral + the scales this app clears, restored"
            {...data}
          />
        )}
        {showTokens && (
          <Panel
            design="tokens"
            title="Project tokens"
            note="dropped in as-is — nothing restored"
            {...data}
          />
        )}
      </div>

      <section className="lab-notes">
        <h2>The finding that is not about colour</h2>
        <p>
          Both panels clip. The rail&rsquo;s scroll container is{" "}
          <code>overflow-y-auto</code> with the scrollbar suppressed on all three engines —{" "}
          <code>[&amp;::-webkit-scrollbar]:hidden</code>, <code>[scrollbar-width:none]</code>,{" "}
          <code>[-ms-overflow-style:none]</code>. Against the registry&rsquo;s fourteen rows that is
          a tidy choice that costs nothing. Against this app&rsquo;s rail it swallows the end of the
          list, and because the bar is gone there is no scrollbar, no fade and no arrow to say so.{" "}
          <b>Council and the whole of Pillars simply are not there.</b>
        </p>
        <p>
          Switch <b>Tabs</b> to <b>Registry demo</b> and the strip disappears — which is the point.
          The design is not wrong; it was drawn for a shorter list. Adopting it means picking one of:
          give the scroller a visible scrollbar, add a fade or count at the fold, collapse groups by
          default, or shorten the rail. That is a design decision, and it is the one the paste hides.
        </p>

        <h2>What the two panels differ by</h2>
        <p>
          One class name: <code>prompt-design</code>. Same component, same props, same class list on
          every element inside. The adoption cost was measured, not guessed — the lab was built once
          without that stylesheet and all <b>195</b> class tokens in the paste were looked for in
          the emitted CSS. <b>186 compile. Nine do not</b>, and they are these:
        </p>
        <ul>
          <li>
            <code>shadow-xs</code>, <code>shadow-sm</code>, <code>shadow-xl</code>,{" "}
            <code>shadow-2xl</code> — <code>--shadow-*</code> is cleared and refilled as{" "}
            <code>shadow-raise</code> / <code>shadow-float</code> / <code>shadow-overlay</code>. The
            monogram, the window, both stat cards, the panel, the shortcut chips, the workspace menu
            and the search dialog all lose their elevation.
          </li>
          <li>
            <code>rounded-xl</code> — <code>tokens.css</code> stops at <code>--radius-lg</code>.
            Five of the most visible corners on the page go square.
          </li>
          <li>
            <code>font-sans</code> — <code>--font-*</code> is cleared and refilled as{" "}
            <code>display</code> / <code>body</code> / <code>mono</code>; there is no{" "}
            <code>sans</code>. Note that <code>font-mono</code> <em>does</em> compile — the shortcut
            chips render in Spline Sans Mono rather than in nothing.
          </li>
          <li>
            <code>animate-in fade-in zoom-in-95</code> — <code>tw-animate-css</code> is not
            installed, so the workspace menu and the search dialog cut in hard{" "}
            <em>on both sides</em> unless the lab supplies the keyframe. This is the one item on the
            list that is not the clearing tripwire — it is a missing dependency.
          </li>
        </ul>
        <p>
          Colour is not on that list, because none of it is missing — it is{" "}
          <em>different</em>. Every shadcn-shaped name resolves, and resolves to this app&rsquo;s
          tokens: the left panel is shadcn neutral, the right is <code>tokens.css</code>.
        </p>

        <h2>Where the cyan is allowed to be</h2>
        <p>
          Cyan is the whole of the branding — <code>--accent</code> is <code>#4fd1e0</code> on dark
          and <code>#0b8493</code> on light, and <code>tokens.css</code> calls it{" "}
          <em>&ldquo;the one brand colour. Not a state; never used to mean anything&rdquo;</em>,
          reserved for the wordmark, links and the focus ring. So there is exactly one place in this
          rail it belongs, and the mark is now in it.
        </p>
        <p>
          What that rules out is the more useful half. The paste paints the workspace tile{" "}
          <code>bg-primary</code> and the badges <code>bg-primary/10 text-primary</code> —{" "}
          <code>primary</code> here is <code>--solid</code>, near-white, <b>not</b> cyan, and it has
          to stay that way. A cyan <code>7</code> next to Chats would make the brand colour mean
          &ldquo;seven things are waiting&rdquo;, which is the one thing the token says it must never
          do. So the badges are left exactly as the paste drew them, and only the tile changed.
        </p>
        <p>
          The mark rides with the data, not with the design toggle: switch <b>Tabs</b> to{" "}
          <b>Registry demo</b> and the monogram comes back, because <code>Acme Corp</code> is not
          ours to brand. Worth knowing before adoption: <code>mark.png</code> is 1024&times;1024 and
          694&nbsp;kB to draw a 32&nbsp;px tile. Fine on a bench, wasteful in the app — it wants an
          SVG or a small raster before it ships.
        </p>

        <h2>What the switcher under the mark is for</h2>
        <p>
          In the paste it is a tenant picker — <code>Acme Corp</code>, <code>Personal Workspace</code>,{" "}
          <code>Create Workspace</code>, <code>Pro Plan</code>. <b>None of that exists here.</b>{" "}
          NucleOS is a local desktop núcleo with no tenants and no plans, and the real rail&rsquo;s{" "}
          <code>.nav-brand</code> holds a wordmark and a collapse button and nothing else. An
          earlier version of this lab filled the slot with three invented names, which is worse than
          leaving it empty: it made the component look integrated while showing data that
          corresponds to nothing.
        </p>
        <p>
          It now switches <b>projects</b>, with <code>NucleOS</code> first and separated as the way
          back out to Home. That is not a decoration — it answers something{" "}
          <code>nav.ts</code> left open. The Projects group was promoted from a single item because
          reaching a project cost <em>&ldquo;a list to open and then a choice, on every single
          entry&rdquo;</em>; then the roster rows were made <b>conditional</b>, drawn only while you
          are already in the projects area. That correction was right for the rail&rsquo;s length,
          and it handed the cost back — from the Feed, a project is once again a list and a choice.
          A switcher pinned above the scroll is where that access can live without the roster&rsquo;s
          length pushing Work and Pillars off the bottom.
        </p>
        <p>
          The notes beside each name are the project&rsquo;s autopilot mode — a real field on{" "}
          <code>ProjectSummary</code>, matched to <code>src/preview/daemon.ts</code>. The last row is{" "}
          <code>New project</code> rather than <code>Create Workspace</code>, and that is not a
          rename for taste: <code>/projects/new</code> is a route this app actually has, so the row
          can be wired to something. The paste&rsquo;s label could not be.
        </p>
        <p>
          One thing the lab cannot show you: <code>bg-accent</code>. The paste never uses it, but{" "}
          <code>tailwind.css</code> warns that shadcn spells hover-background{" "}
          <code>accent</code> while this app reserves <code>--accent</code> for the wordmark, links
          and the focus ring — so a sibling component from the same registry will flash cyan. This
          one hovers with <code>bg-black/5 dark:bg-white/5</code> and is clear of that trap.
        </p>
      </section>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<Lab />);
