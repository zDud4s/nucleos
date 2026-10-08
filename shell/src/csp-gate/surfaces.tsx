import type { ReactNode } from "react";
import { useEffect, useState } from "react";

import { WorkflowCanvas } from "../canvas/WorkflowCanvas";
import type { GraphEdge, GraphNode } from "../data/workflow-graph";
import {
  CommandDialog,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/ui/vendor/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/ui/vendor/dialog";
import { Sparkline } from "@/ui/Sparkline";
import { ForceGraph } from "../brain/ForceGraph";
import type { GModel } from "../brain/graph-types";
import type { Placed } from "../brain/hit-test";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/ui/vendor/dropdown-menu";

/**
 * The surfaces the CSP gate puts under the production policy, and why each one is here.
 *
 * **This is a list of LIBRARIES, not a list of screens**, and the distinction is what keeps it
 * short enough to stay true. A Content-Security-Policy refusal is a property of how a dependency
 * writes style — a `<style>` element it builds at runtime, or a `style=` attribute it puts into
 * markup — so covering one screen that uses xyflow covers every screen that uses xyflow. Six
 * libraries in this app can write style that way, and every one of them has a surface below.
 *
 * The list is meant to be annoying in the same way `app/nav.test.ts` is: **adding a dependency
 * that renders should mean adding a row here.** Nothing enforces that, and nothing can — the gate
 * cannot know what a new package does before somebody thinks about it. What it can do is make the
 * omission visible, which is why each row says what it is standing in for rather than just naming
 * a component.
 *
 * Two absences, both deliberate:
 *
 * - **`FleetCanvas`** is not here. It is xyflow, and xyflow is already covered by the workflow
 *   canvas; a second surface of the same library buys a second chance to be slow and no new
 *   information.
 * - **Radix Tabs** is not here, and it is the same argument in a different library: Tabs rides the
 *   roving-focus machinery the dropdown already exercises and does not position with Popper, so it
 *   can emit nothing the `menu` surface has not already proved.
 *
 * This header used to say `@visx/*` was absent because nothing in `src/` imported it, and that
 * it would want a row when something did. Something does: `ui/Sparkline.tsx`. The row is `charts`,
 * below, and this paragraph is rewritten rather than left standing as a comment that has quietly
 * become false — which is the failure mode a list like this dies of.
 */

/** One thing under test: mounted alone, on its own page load, so a refusal has one suspect. */
export interface Surface {
  /** What the driver passes as `?surface=`. */
  name: string;
  /** The library this stands in for, and what it does that a CSP could refuse. */
  why: string;
  /**
   * Mounted with `done` to call when it has finished exercising itself.
   *
   * The exercising lives in the surface and not in the driver on purpose: what counts as "opened"
   * or "animating" is a fact about the component, and a driver that clicked buttons by their text
   * would go quietly green the day a label changed.
   */
  Component: (props: { done: () => void }) => ReactNode;
}

/** Long enough for a spring to run and a portal to settle; short enough to run four of them. */
const SETTLE = 1200;

function useDoneAfter(done: () => void, delay = SETTLE) {
  useEffect(() => {
    const timer = window.setTimeout(done, delay);
    return () => window.clearTimeout(timer);
  }, [done, delay]);
}

/* -------------------------------------------------------------------------- */

const GRAPH_NODES: GraphNode[] = [
  {
    id: "plan",
    type: "agent",
    role: "plain",
    label: "plan",
    disabled: false,
    overridden: true,
    fields: [{ name: "model", value: "opus", origin: "sonnet" }],
  },
  { id: "build", type: "agent", role: "plain", label: "build", disabled: false, overridden: false, fields: [] },
  {
    id: "gate",
    type: "command",
    role: "gate",
    label: "gate",
    disabled: false,
    overridden: false,
    fields: [{ name: "command", value: "cargo test" }],
  },
  { id: "each", type: "fan", role: "plain", label: "each package", disabled: false, overridden: false, fields: [] },
  { id: "review", type: "decision", role: "plain", label: "review", disabled: true, overridden: true, fields: [] },
];

const GRAPH_EDGES: GraphEdge[] = [
  { from: "plan", to: "build" },
  { from: "build", to: "gate" },
  { from: "gate", to: "each", verdict: "pass" },
  { from: "gate", to: "build", verdict: "fail" },
  { from: "each", to: "review", when: "changed" },
];

/**
 * `motion` and `@xyflow/react`.
 *
 * All four node kinds and both edge kinds, because the edge labels go through xyflow's
 * `EdgeLabelRenderer` — a portal that positions with a transform, which is the shape most likely
 * to be written as a `style` attribute by a library that did it the other way. `running` is set
 * from the first frame so the spring is live, then cleared, so both branches of the `animate` prop
 * are exercised rather than only the resting one.
 */
function WorkflowSurface({ done }: { done: () => void }) {
  const [running, setRunning] = useState<string | null>("build");
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    const stop = window.setTimeout(() => setRunning(null), 700);
    const pick = window.setTimeout(() => setSelected("gate"), 300);
    return () => {
      window.clearTimeout(stop);
      window.clearTimeout(pick);
    };
  }, []);
  useDoneAfter(done, 1800);

  return (
    <div className="p-6">
      <WorkflowCanvas
        nodes={GRAPH_NODES}
        edges={GRAPH_EDGES}
        running={running}
        selected={selected}
        onSelect={setSelected}
      />
    </div>
  );
}

/**
 * `radix-ui`, through the one thing in it that has already bitten this app.
 *
 * An open modal dialog is what turns on `react-remove-scroll`, and `react-remove-scroll` locks
 * scrolling by **injecting a `<style>` element**. It is the reason this gate exists in the shape
 * it does: it works in `tauri dev`, where `devCsp` allows inline style, and it is refused in a
 * packaged build. Mounted `open` from the first render rather than clicked open, because a
 * trigger is a screen detail and the lock is what is under test.
 */
function DialogSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <Dialog open>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>An open modal dialog</DialogTitle>
          <DialogDescription>
            Open from the first render, so the scroll lock is engaged.
          </DialogDescription>
        </DialogHeader>
        <p>Body, long enough that the document behind it would scroll.</p>
      </DialogContent>
    </Dialog>
  );
}

/**
 * `radix-ui` again, on the other half of it: Popper, not Dialog.
 *
 * A dropdown positions its content by measuring the trigger and writing the result somewhere. If
 * that somewhere is a `style` attribute in markup rather than the CSSOM, `style-src-attr` refuses
 * it and the menu lands in the top-left corner of the window in a packaged build only. Worth its
 * own surface because it is a different code path from the dialog's, not a second dialog.
 */
function MenuSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <div className="p-24">
      <DropdownMenu open>
        <DropdownMenuTrigger>a menu</DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuLabel>positioned by Popper</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem>first</DropdownMenuItem>
          <DropdownMenuItem>second</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/**
 * `cmdk`, which is the ⌘K palette slice 6 put in front of every project command.
 *
 * It arrives wrapped in a Radix dialog, so this surface covers both at once — but it is not the
 * dialog surface repeated: cmdk does its own filtering and its own list virtualisation, and both
 * are places a library writes style per keystroke.
 */
function PaletteSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <CommandDialog open>
      <CommandInput placeholder="type a command" />
      <CommandList>
        <CommandGroup heading="gates">
          <CommandItem>gate</CommandItem>
          <CommandItem>fmt</CommandItem>
          <CommandItem>typecheck</CommandItem>
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}

/**
 * `@visx/*`, through the one chart this app draws.
 *
 * A chart library is this gate's canonical case: it computes geometry at render time and the
 * question is always whether it writes the result into the CSSOM or into a `style` attribute in
 * markup. visx composes d3's scales into React elements rather than mutating the DOM, so it should
 * be clean — "should be" is precisely what a gate is for.
 *
 * Two sparklines rather than one, because the component has two branches and only one of them
 * emits an `<svg>`: a series with points, and the dashed rail it draws for a series too short to
 * plot. A surface that exercised only the happy branch would go green on half the component.
 */
function ChartSurface({ done }: { done: () => void }) {
  useDoneAfter(done);

  return (
    <div className="p-6">
      <Sparkline values={[0, 2, 1, 4, 3, 6, 2, 5]} label="the 8 runs in the window" />
      <Sparkline values={[]} label="no run in the window" />
    </div>
  );
}

const FORCE_MODEL: GModel = {
  nodes: [
    { id: "n:1", kind: "note", ref: "1", label: "Rust owns the state", bucket: "in_force", missing: false, degree: 3 },
    {
      id: "k:2",
      kind: "knowledge",
      ref: "2",
      label: "prefer small diffs",
      layer: "procedural",
      bucket: "in_force",
      missing: false,
      degree: 2,
    },
    {
      id: "k:3",
      kind: "knowledge",
      ref: "3",
      label: "a proposal",
      layer: "semantic",
      bucket: "proposed",
      missing: false,
      degree: 1,
    },
    { id: "project:p1", kind: "project", ref: "p1", label: "p1", bucket: "unknown", missing: false, degree: 2 },
    { id: "contact:c1", kind: "contact", ref: "c1", label: "a contact", bucket: "unknown", missing: false, degree: 1 },
    { id: "n:9", kind: "note", ref: "9", label: "deleted note", bucket: "unknown", missing: true, degree: 1 },
  ],
  edges: [
    { id: "relates|n:1|k:2", source: "n:1", target: "k:2", type: "relates" },
    { id: "supports|n:1|contact:c1", source: "n:1", target: "contact:c1", type: "supports" },
    { id: "details|n:1|n:9", source: "n:1", target: "n:9", type: "details" },
    { id: "scope|k:2|project:p1", source: "k:2", target: "project:p1", type: "scope" },
    { id: "scope|k:3|project:p1", source: "k:3", target: "project:p1", type: "scope" },
  ],
};

/**
 * `d3-force`, `d3-zoom`, `d3-drag` (and `d3-selection` under them), through the Brain graph.
 *
 * The graph paints a canvas, so the risk is not its pixels but its plumbing: d3-zoom and d3-drag
 * set `touch-action`, tap-highlight and user-select as they bind, and the component sizes its
 * canvas. All of that must go through the CSSOM and never through a `style` attribute in markup or
 * a runtime `<style>`. Every node shape is on screen (filled, hollow proposed, dashed missing,
 * selected ring), and one hover is simulated once the first frame says where a node is, so the
 * neighbour-lighting branch runs too.
 */
function ForceGraphSurface({ done }: { done: () => void }) {
  const [selected, setSelected] = useState<string | null>("k:2");
  const [placed, setPlaced] = useState<Placed[] | null>(null);

  useEffect(() => {
    if (!placed) return;
    const canvas = document.querySelector<HTMLCanvasElement>(".brain-force-canvas");
    const target = placed.find((p) => p.id === "n:1");
    if (!canvas || !target) return;
    const rect = canvas.getBoundingClientRect();
    canvas.dispatchEvent(
      new PointerEvent("pointermove", {
        bubbles: true,
        clientX: rect.left + target.x,
        clientY: rect.top + target.y,
      }),
    );
  }, [placed]);
  useDoneAfter(done, 1800);

  return (
    <div className="p-6">
      <ForceGraph
        model={FORCE_MODEL}
        selected={selected}
        onSelect={setSelected}
        onLayout={(next) => setPlaced((current) => current ?? next)}
      />
    </div>
  );
}

export const SURFACES: Surface[] = [
  {
    name: "workflow",
    why: "motion (springs, layout) and @xyflow/react (nodes, edges, the edge-label portal)",
    Component: WorkflowSurface,
  },
  {
    name: "dialog",
    why: "radix-ui's modal dialog, which turns on react-remove-scroll — the one that injects a <style>",
    Component: DialogSurface,
  },
  {
    name: "menu",
    why: "radix-ui's Popper positioning, a different path from the dialog's",
    Component: MenuSurface,
  },
  {
    name: "palette",
    why: "cmdk, the ⌘K palette, filtering and rendering inside a dialog",
    Component: PaletteSurface,
  },
  {
    name: "charts",
    why: "@visx/{group,scale,shape}, which compute geometry per render — both branches of Sparkline",
    Component: ChartSurface,
  },
  {
    name: "force-graph",
    why: "d3-force / d3-zoom / d3-drag on a canvas — no runtime style, proven here",
    Component: ForceGraphSurface,
  },
];
