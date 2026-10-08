import { useEffect, useLayoutEffect, useRef } from "react";
import {
  forceCenter,
  forceCollide,
  forceLink,
  forceManyBody,
  forceSimulation,
  type SimulationLinkDatum,
  type SimulationNodeDatum,
} from "d3-force";
import { zoom as d3zoom, zoomIdentity, type ZoomTransform } from "d3-zoom";
import { drag as d3drag } from "d3-drag";
import { select } from "d3-selection";

import type { GEdgeType, GModel, GNode } from "./graph-types";
import { colorToken, neighbours, nodeRadius } from "./graph-util";
import { hitTest, labelVisible, type Placed } from "./hit-test";
import "./force-graph.css";

/**
 * The Brain graph on a `<canvas>`: dots, springs, hover lighting a node's neighbours, labels once
 * zoomed in. Obsidian's graph view is the model.
 *
 * **Why a canvas, and why no style ever goes through an attribute.** The production CSP is
 * `style-src 'self'` with no `'unsafe-inline'`, so a `style="…"` string, `setAttribute("style")` or
 * a `<style>` built at runtime is refused in the shipped app (the CSP gate's `force-graph` surface
 * proves this component stays clear of all three). The canvas paints its own pixels, its palette
 * comes from the `--brain-*` properties in `force-graph.css` read back through `getComputedStyle`,
 * and the one size it needs — its height — is set through the CSSOM (`element.style.height`),
 * which a CSP does not police. d3-zoom and d3-drag write their few properties the same way.
 *
 * Everything that is arithmetic — which node is under the pointer, when a label shows — lives in
 * `hit-test.ts`, so this file is the wiring and the painting.
 */
export interface ForceGraphProps {
  model: GModel;
  selected: string | null;
  onSelect: (id: string) => void;
  /** The local graph: smaller, no zoom controls, no wheel zoom, labels always on. */
  compact?: boolean;
  /** CSS pixels. Default 560, or 220 when compact. */
  height?: number;
  /**
   * Called after every frame with each node as DRAWN — screen position in CSS pixels relative to
   * the canvas, and on-screen radius. A test hook (and the CSP surface's): it is how a caller
   * knows where to point without reaching into the simulation.
   */
  onLayout?: (placed: Placed[]) => void;
}

type SimNode = GNode & SimulationNodeDatum;
type SimLink = SimulationLinkDatum<SimNode> & { id: string; type: GEdgeType };

interface Engine {
  setModel(model: GModel, compact: boolean): void;
  redraw(): void;
  resize(): void;
  fit(): void;
  zoomBy(factor: number): void;
}

/** jsdom and a not-yet-laid-out container both measure 0; draw at something rather than nothing. */
const FALLBACK_WIDTH = 600;
/** Fit the view once the layout has had this many ticks to settle. */
const FIT_AFTER_TICKS = 120;
const LABEL_MAX = 40;
/** The local graph sits in a 23rem panel: a 40-character label ran off its edge. */
const LOCAL_LABEL_MAX = 22;

/**
 * Under reduced motion the layout is settled before it is drawn rather than animated into place:
 * the drift of a settling graph is exactly the motion the setting asks to leave out.
 */
function prefersReducedMotion(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function ForceGraph({ model, selected, onSelect, compact = false, height, onLayout }: ForceGraphProps) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const engineRef = useRef<Engine | null>(null);

  // The engine lives outside React's render cycle; it reads the latest props through these.
  const resolvedHeight = height ?? (compact ? 220 : 560);
  const live = useRef({ model, selected, onSelect, compact, height: resolvedHeight, onLayout });
  useLayoutEffect(() => {
    live.current = { model, selected, onSelect, compact, height: resolvedHeight, onLayout };
  });

  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    const ctx = canvas.getContext("2d");
    const canvasSel = select<HTMLCanvasElement, unknown>(canvas);

    let width = 0;
    let height = 0;
    let dpr = 1;
    let transform: ZoomTransform = zoomIdentity;
    let centred = false;
    let nodes: SimNode[] = [];
    let links: SimLink[] = [];
    let byId = new Map<string, SimNode>();
    let nodeKey = "";
    let hovered: string | null = null;
    let lit: Set<string> | null = null;
    let ticks = 0;
    let fitted = false;
    let frame = 0;
    let disposed = false;

    const linkForce = forceLink<SimNode, SimLink>([]).id((d) => d.id);
    const chargeForce = forceManyBody<SimNode>();
    const sim = forceSimulation<SimNode, SimLink>([])
      .force("link", linkForce)
      .force("charge", chargeForce)
      .force("center", forceCenter())
      .force("collide", forceCollide<SimNode>((d) => nodeRadius(d.degree) + 2))
      .stop();

    const placed = (): Placed[] =>
      nodes.map((n) => ({ id: n.id, x: n.x ?? 0, y: n.y ?? 0, r: nodeRadius(n.degree) }));

    const local = (event: MouseEvent) => {
      const rect = canvas.getBoundingClientRect();
      return { x: event.clientX - rect.left, y: event.clientY - rect.top };
    };

    const requestDraw = () => {
      if (frame || disposed) return;
      frame = window.requestAnimationFrame(draw);
    };

    function draw() {
      frame = 0;
      if (disposed) return;
      const { selected: sel, compact: small, onLayout: report } = live.current;
      const t = transform;
      if (ctx) paint(ctx, t, sel, small);
      report?.(
        nodes.map((n) => ({
          id: n.id,
          x: t.applyX(n.x ?? 0),
          y: t.applyY(n.y ?? 0),
          r: nodeRadius(n.degree) * t.k,
        })),
      );
    }

    function paint(c: CanvasRenderingContext2D, t: ZoomTransform, sel: string | null, small: boolean) {
      // Every token read once per frame: a theme switch shows on the next frame, and a frame
      // never asks the style engine twice for the same colour.
      const style = getComputedStyle(canvas!);
      const cache = new Map<string, string>();
      const colour = (token: string, fallback: string) => {
        let value = cache.get(token);
        if (value === undefined) {
          value = style.getPropertyValue(token).trim() || fallback;
          cache.set(token, value);
        }
        return value;
      };
      const k = t.k;

      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.clearRect(0, 0, width, height);
      c.setTransform(dpr * k, 0, 0, dpr * k, dpr * t.x, dpr * t.y);

      // Edges: the quiet ones in one path, then the ones touching the hovered (or, with nothing
      // hovered, the selected) node drawn stronger on top.
      const focus = hovered ?? sel;
      const strong: SimLink[] = [];
      c.globalAlpha = lit ? 0.15 : 1;
      c.strokeStyle = colour("--brain-edge", "rgba(152, 161, 178, 0.3)");
      c.lineWidth = 1 / k;
      c.beginPath();
      for (const l of links) {
        const s = l.source as SimNode;
        const d = l.target as SimNode;
        if (typeof s !== "object" || typeof d !== "object") continue;
        if (focus !== null && (s.id === focus || d.id === focus)) {
          strong.push(l);
          continue;
        }
        c.moveTo(s.x ?? 0, s.y ?? 0);
        c.lineTo(d.x ?? 0, d.y ?? 0);
      }
      c.stroke();
      c.globalAlpha = 1;
      c.strokeStyle = colour("--brain-edge-strong", "rgba(231, 234, 240, 0.75)");
      c.lineWidth = 1.5 / k;
      c.beginPath();
      for (const l of strong) {
        const s = l.source as SimNode;
        const d = l.target as SimNode;
        c.moveTo(s.x ?? 0, s.y ?? 0);
        c.lineTo(d.x ?? 0, d.y ?? 0);
      }
      c.stroke();

      // Nodes, in array order — the same order `hitTest` treats as bottom-to-top.
      for (const n of nodes) {
        const x = n.x ?? 0;
        const y = n.y ?? 0;
        const r = nodeRadius(n.degree);
        const fill = colour(colorToken(n), "#8b94a3");
        c.globalAlpha = lit && !lit.has(n.id) ? 0.15 : 1;
        c.beginPath();
        c.arc(x, y, r, 0, Math.PI * 2);
        if (n.missing) {
          c.setLineDash([3 / k, 2 / k]);
          c.strokeStyle = colour("--brain-missing", "#808a9a");
          c.lineWidth = 1.5 / k;
          c.stroke();
          c.setLineDash([]);
        } else if (n.bucket === "proposed") {
          c.strokeStyle = fill;
          c.lineWidth = 1.5 / k;
          c.stroke();
        } else {
          c.fillStyle = fill;
          c.fill();
        }
        if (n.id === sel) {
          c.beginPath();
          c.arc(x, y, r + 3 / k, 0, Math.PI * 2);
          c.strokeStyle = colour("--brain-ring", "#4fd1e0");
          c.lineWidth = 2 / k;
          c.stroke();
        }
      }

      // Labels, at a constant size on screen whatever the zoom.
      const family = colour("--font-body", "system-ui, sans-serif");
      c.font = `${11 / k}px ${family}`;
      c.textBaseline = "middle";
      for (const n of nodes) {
        const isHovered = n.id === hovered;
        const isSelected = n.id === sel;
        if (!labelVisible(k, isHovered, isSelected, small)) continue;
        c.globalAlpha = lit && !lit.has(n.id) ? 0.15 : 1;
        c.fillStyle =
          isHovered || isSelected
            ? colour("--brain-label-strong", "#e7eaf0")
            : colour("--brain-label", "#98a1b2");
        const max = small ? LOCAL_LABEL_MAX : LABEL_MAX;
        const text = n.label.length > max ? `${n.label.slice(0, max - 1)}…` : n.label;
        c.fillText(text, (n.x ?? 0) + nodeRadius(n.degree) + 4 / k, n.y ?? 0);
      }
      c.globalAlpha = 1;
    }

    const setHovered = (id: string | null) => {
      if (id === hovered) return;
      hovered = id;
      lit = id ? neighbours(live.current.model, id, 1) : null;
      canvas.classList.toggle("is-hovering", id !== null);
      requestDraw();
    };

    const fit = () => {
      if (nodes.length === 0 || width === 0) return;
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      for (const n of nodes) {
        const r = nodeRadius(n.degree);
        minX = Math.min(minX, (n.x ?? 0) - r);
        minY = Math.min(minY, (n.y ?? 0) - r);
        maxX = Math.max(maxX, (n.x ?? 0) + r);
        maxY = Math.max(maxY, (n.y ?? 0) + r);
      }
      const pad = live.current.compact ? 12 : 32;
      const fitK = Math.min(
        (width - 2 * pad) / Math.max(maxX - minX, 1),
        (height - 2 * pad) / Math.max(maxY - minY, 1),
      );
      const k = Math.max(0.2, Math.min(live.current.compact ? 2 : 1.6, fitK));
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      canvasSel.call(
        zoomBehaviour.transform,
        zoomIdentity.translate(width / 2, height / 2).scale(k).translate(-cx, -cy),
      );
    };

    sim.on("tick", () => {
      ticks += 1;
      if (!fitted && ticks >= FIT_AFTER_TICKS) {
        fitted = true;
        fit();
      }
      requestDraw();
    });
    sim.on("end", () => {
      if (!fitted) {
        fitted = true;
        fit();
      }
      requestDraw();
    });

    // Drag goes on BEFORE zoom: when the pointer lands on a node, d3-drag stops the event and the
    // zoom never sees it; on empty canvas there is no subject, and the press becomes a pan.
    const dragBehaviour = d3drag<HTMLCanvasElement, unknown, SimNode | undefined>()
      .container(canvas)
      .subject((event: { x: number; y: number }) => {
        const id = hitTest(placed(), { x: event.x, y: event.y }, transform);
        return id ? byId.get(id) : undefined;
      })
      .on("start", (event: { active: number; subject: SimNode }) => {
        canvas.classList.add("is-dragging");
        if (!event.active) sim.alphaTarget(0.3).restart();
        event.subject.fx = event.subject.x;
        event.subject.fy = event.subject.y;
      })
      .on("drag", (event: { dx: number; dy: number; subject: SimNode }) => {
        // dx/dy are screen pixels; the node lives in world units.
        const s = event.subject;
        s.fx = (s.fx ?? s.x ?? 0) + event.dx / transform.k;
        s.fy = (s.fy ?? s.y ?? 0) + event.dy / transform.k;
      })
      .on("end", (event: { active: number; subject: SimNode }) => {
        canvas.classList.remove("is-dragging");
        if (!event.active) sim.alphaTarget(0);
        event.subject.fx = null;
        event.subject.fy = null;
      });

    const zoomBehaviour = d3zoom<HTMLCanvasElement, unknown>()
      .scaleExtent([0.2, 4])
      .filter((event: Event & { ctrlKey?: boolean; button?: number }) => {
        if (event.type === "wheel") return !live.current.compact;
        return !event.ctrlKey && !event.button;
      })
      .on("zoom", (event: { transform: ZoomTransform }) => {
        transform = event.transform;
        requestDraw();
      });

    canvasSel.call(dragBehaviour).call(zoomBehaviour);

    const onPointerMove = (event: PointerEvent) => {
      setHovered(hitTest(placed(), local(event), transform));
    };
    const onPointerLeave = () => setHovered(null);
    // A drag or a pan that moved swallows the click that follows it (d3 does that), so a click
    // that reaches here is a click.
    const onClick = (event: MouseEvent) => {
      const id = hitTest(placed(), local(event), transform);
      if (id) live.current.onSelect(id);
    };
    canvas.addEventListener("pointermove", onPointerMove);
    canvas.addEventListener("pointerleave", onPointerLeave);
    canvas.addEventListener("click", onClick);

    const resize = () => {
      const w = wrap.clientWidth || FALLBACK_WIDTH;
      const h = live.current.height;
      const ratio = window.devicePixelRatio || 1;
      if (w === width && h === height && ratio === dpr) return;
      width = w;
      height = h;
      dpr = ratio;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      canvas.style.height = `${h}px`; // CSSOM, not an attribute: allowed under the CSP.
      zoomBehaviour.extent([
        [0, 0],
        [w, h],
      ]);
      if (!centred) {
        centred = true;
        canvasSel.call(zoomBehaviour.transform, zoomIdentity.translate(w / 2, h / 2));
      }
      requestDraw();
    };
    const observer = new ResizeObserver(() => resize());
    observer.observe(wrap);
    resize();

    engineRef.current = {
      setModel(next, small) {
        const previous = byId;
        // Positions carry over by id, so a filter change rearranges the graph instead of
        // exploding it from the origin.
        nodes = next.nodes.map((n) => {
          const old = previous.get(n.id);
          return old ? { ...n, x: old.x, y: old.y, vx: old.vx, vy: old.vy } : { ...n };
        });
        byId = new Map(nodes.map((n) => [n.id, n]));
        links = next.edges
          .filter((e) => byId.has(e.source) && byId.has(e.target))
          .map((e) => ({ id: e.id, type: e.type, source: e.source, target: e.target }));
        const key = nodes.map((n) => n.id).join("\n");
        if (key !== nodeKey) {
          nodeKey = key;
          ticks = 0;
          fitted = false;
        }
        if (hovered && !byId.has(hovered)) hovered = null;
        lit = hovered ? neighbours(next, hovered, 1) : null;

        // Links cleared first: re-initialising the old links against the new node set would
        // look up ids that may no longer exist.
        linkForce.links([]);
        sim.nodes(nodes);
        linkForce.links(links).distance(small ? 40 : 60);
        chargeForce.strength(small ? -60 : -120);
        if (prefersReducedMotion()) {
          sim.alpha(0.5).stop();
          sim.tick(Math.ceil(Math.log(sim.alphaMin() / 0.5) / Math.log(1 - sim.alphaDecay())));
          fitted = true;
          fit();
        } else {
          sim.alpha(0.5).restart();
        }
        requestDraw();
      },
      redraw: requestDraw,
      resize,
      fit,
      zoomBy(factor) {
        canvasSel.call(zoomBehaviour.scaleBy, factor, [width / 2, height / 2]);
      },
    };

    return () => {
      disposed = true;
      sim.stop();
      sim.on("tick", null).on("end", null);
      if (frame) window.cancelAnimationFrame(frame);
      frame = 0;
      observer.disconnect();
      canvas.removeEventListener("pointermove", onPointerMove);
      canvas.removeEventListener("pointerleave", onPointerLeave);
      canvas.removeEventListener("click", onClick);
      canvasSel.on(".drag", null).on(".zoom", null);
      engineRef.current = null;
    };
  }, []);

  useEffect(() => {
    engineRef.current?.setModel(model, compact);
  }, [model, compact]);

  useEffect(() => {
    engineRef.current?.redraw();
  }, [selected]);

  useEffect(() => {
    engineRef.current?.resize();
  }, [resolvedHeight]);

  return (
    <div ref={wrapRef} className="brain-force">
      <canvas
        ref={canvasRef}
        className="brain-force-canvas"
        tabIndex={0}
        role="img"
        aria-label={`Brain graph, ${model.nodes.length} nodes`}
      />
      {!compact && (
        <div className="brain-force-controls">
          <button
            type="button"
            className="brain-force-control"
            aria-label="Zoom in"
            onClick={() => engineRef.current?.zoomBy(1.3)}
          >
            +
          </button>
          <button
            type="button"
            className="brain-force-control"
            aria-label="Zoom out"
            onClick={() => engineRef.current?.zoomBy(1 / 1.3)}
          >
            −
          </button>
          <button
            type="button"
            className="brain-force-control"
            aria-label="Fit to view"
            onClick={() => engineRef.current?.fit()}
          >
            Fit
          </button>
        </div>
      )}
    </div>
  );
}
