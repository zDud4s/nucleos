import { useEffect, useRef } from "react";
import type { ConversationView } from "../data/conversation";

/**
 * The conversation's orb: latitude lines flowing pole to pole over a sphere, nearer strokes brighter.
 *
 * Drawn on a 2D canvas rather than WebGL. The look is a set of wobbling ellipses with a depth fade,
 * and a 3D engine for that would be several hundred kilobytes and a GL context on a page that already
 * holds a microphone open. The projection below is the one a camera at z = 8 with a 45° field of view
 * would give, which is all the original effect ever used of its scene.
 *
 * It is decoration, and says so to assistive technology: the meter under it carries the same reading
 * as a value and a sentence. What the orb adds is that it moves with the voice — the squiggle grows
 * and quickens with the level, measured against the bar that would open a turn, and the lines take
 * the hearing badge's colour while a turn is being heard.
 */
export function VoiceOrb({
  phase,
  level,
  threshold,
}: Pick<ConversationView, "phase" | "level" | "threshold">) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /* Read by the frame loop, which outlives every render; a prop captured by its closure would freeze
     the orb at whatever it heard first. */
  const inputRef = useRef({ phase, level, threshold });
  inputRef.current = { phase, level, threshold };

  useEffect(() => {
    const canvas = canvasRef.current;
    // No `matchMedia` means no layout (jsdom), and with no layout there is nothing to draw on.
    if (canvas === null || typeof window.matchMedia !== "function") return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
    const state = { flow: 0, wobble: 0, energy: 0, last: performance.now() };
    let frame = 0;

    const draw = (now: number) => {
      const dt = Math.min((now - state.last) / 1000, 0.1);
      state.last = now;
      const { phase, level, threshold } = inputRef.current;
      const target = phase === "off" ? 0 : Math.min(level / Math.max(threshold, 0.01), 1.5);
      // Eased, so a level that arrives in steps reads as a swell rather than a twitch.
      state.energy += (target - state.energy) * Math.min(dt * 6, 1);
      const still = reduced.matches;
      if (!still) {
        state.flow += dt * (phase === "thinking" ? 2.5 : phase === "off" ? 0.5 : 1);
        state.wobble += dt * (BASE.squiggleSpeed + 3 * state.energy);
      }
      paint(ctx, canvas, state.flow, state.wobble, state.energy, phase === "off" ? 0.45 : 1);
      if (!still) frame = requestAnimationFrame(draw);
    };

    const resize = () => {
      const box = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(box.width * dpr);
      canvas.height = Math.round(box.height * dpr);
      if (reduced.matches) draw(performance.now());
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    // A preference changed while the page is open is obeyed: the loop stops, or starts again.
    const onPreference = () => {
      cancelAnimationFrame(frame);
      state.last = performance.now();
      frame = requestAnimationFrame(draw);
    };
    reduced.addEventListener("change", onPreference);
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      reduced.removeEventListener("change", onPreference);
    };
  }, []);

  // Under reduced motion there is no loop, so each new reading is drawn once, when it arrives.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (canvas === null || typeof window.matchMedia !== "function") return;
    if (!window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;
    const energy = phase === "off" ? 0 : Math.min(level / Math.max(threshold, 0.01), 1.5);
    paint(ctx, canvas, 0, 0, energy, phase === "off" ? 0.45 : 1);
  }, [phase, level, threshold]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden="true"
      className="voice-orb"
      data-hearing={phase === "hearing" ? "" : undefined}
    />
  );
}

/** The resting shape: twenty lines, a twenty-second pole-to-pole cycle, a slight squiggle. */
const BASE = {
  lines: 20,
  radius: 1.5,
  cycle: 20,
  points: 96,
  squiggleAmount: 0.04,
  squiggleFrequency: 4,
  squiggleSpeed: 2,
  lineWidth: 2,
};

/** Where the camera stands, in sphere units; nearer points spread a little, as they would through a lens. */
const CAMERA_Z = 8;
/** How many opacity steps the depth fade is quantised into — one stroke per step instead of one per segment. */
const STEPS = 10;

/* One set of buffers, reused every frame: there is one orb on the one page that shows it. */
const SPAN = BASE.points + 1;
const xs = new Float32Array(BASE.lines * SPAN);
const ys = new Float32Array(BASE.lines * SPAN);
const alphas = new Float32Array(BASE.lines * SPAN);

function paint(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  flow: number,
  wobble: number,
  energy: number,
  dim: number,
) {
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  if (width === 0 || height === 0) return;

  const dpr = window.devicePixelRatio || 1;
  const scale = (Math.min(width, height) / 2) * 0.78 / BASE.radius;
  const cx = width / 2;
  const cy = height / 2;
  const amount = BASE.squiggleAmount + 0.08 * energy;
  const freq = BASE.squiggleFrequency;
  const brightness = dim * (0.8 + 0.2 * Math.min(energy, 1));

  // The colour is the canvas's own CSS `color`, so the theme and the hearing colour — and the
  // transition between them — come from the stylesheet rather than from a literal here.
  ctx.strokeStyle = getComputedStyle(canvas).color;
  ctx.lineWidth = BASE.lineWidth * dpr;
  // Round on purpose: where a line crosses from one opacity band to the next, the two caps overlap
  // and leave a faint bead. Butt caps remove it, and were tried; the beads are the look we kept.
  ctx.lineCap = "round";
  ctx.lineJoin = "round";

  // Project every line once, then stroke it in opacity bands.
  for (let line = 0; line < BASE.lines; line++) {
    const turn = (line / BASE.lines) * Math.PI;
    const cosR = Math.cos(turn);
    const sinR = Math.sin(turn);
    const offset = (line / BASE.lines) * BASE.cycle;
    const latitude = (((flow + offset) % BASE.cycle) / BASE.cycle) * Math.PI;
    const ring = Math.sin(latitude) * BASE.radius;
    const height0 = Math.cos(latitude) * BASE.radius;

    const base = line * SPAN;
    for (let i = 0; i < BASE.points; i++) {
      const angle = (i / BASE.points) * Math.PI * 2;
      const squiggle = Math.sin(angle * freq + wobble + line * 0.5) * amount;
      const radial = Math.cos(angle * freq * 1.3 + wobble * 0.8) * amount * 0.5;
      const r = ring + (squiggle + radial) * ring;
      const lift = Math.sin(angle * freq * 0.7 + wobble * 1.2) * amount * 0.4;

      const x = Math.cos(angle) * r;
      const y = height0 + lift * ring;
      const z = Math.sin(angle) * r;
      const worldX = x * cosR + z * sinR;
      const worldZ = -x * sinR + z * cosR;
      const lens = CAMERA_Z / (CAMERA_Z - worldZ);

      xs[base + i] = cx + worldX * lens * scale;
      ys[base + i] = cy - y * lens * scale;
      alphas[base + i] = 0.15 + 0.85 * ((worldZ / BASE.radius + 1) / 2);
    }
    // Close the loop on the first point exactly, or a hairline gap shows where it starts.
    xs[base + BASE.points] = xs[base]!;
    ys[base + BASE.points] = ys[base]!;
    alphas[base + BASE.points] = alphas[base]!;
  }

  for (let step = 0; step < STEPS; step++) {
    ctx.beginPath();
    let any = false;
    for (let line = 0; line < BASE.lines; line++) {
      const base = line * SPAN;
      let open = false;
      for (let i = base; i < base + BASE.points; i++) {
        const a = (alphas[i]! + alphas[i + 1]!) / 2;
        if (Math.min(Math.floor(a * STEPS), STEPS - 1) !== step) {
          open = false;
          continue;
        }
        if (!open) ctx.moveTo(xs[i]!, ys[i]!);
        ctx.lineTo(xs[i + 1]!, ys[i + 1]!);
        open = true;
        any = true;
      }
    }
    if (!any) continue;
    ctx.globalAlpha = ((step + 0.5) / STEPS) * brightness;
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
}
