import { useEffect, useRef } from "react";
import { ANSWER_BANDS, SILENT_TAP, type AnswerTap } from "../data/answer-tap";
import type { ConversationView } from "../data/conversation";
import { DIM, FLOW, orbBreath, orbDrive } from "./VoiceRoom";

/**
 * The conversation's orb: latitude lines flowing pole to pole over a sphere, nearer strokes brighter,
 * with a small dark ball at its heart.
 *
 * Drawn on a 2D canvas rather than WebGL. The look is a set of wobbling ellipses with a depth fade,
 * and a 3D engine for that would be several hundred kilobytes and a GL context on a page that already
 * holds a microphone open. The projection below is the one a camera at z = 8 with a 45° field of view
 * would give, which is all the original effect ever used of its scene.
 *
 * The orb is NucleOS's voice. It moves when the answer plays — the shell quickens and swells with the
 * answer's loudness, and an outline rises round the dark ball in the shape of the answer's spectrum —
 * and it holds still while you talk. It is still decoration, and says so to assistive technology:
 * the meter beside it carries the microphone's reading as a value and a sentence. Without a tap, or
 * under reduced motion, there is no sound to follow and no loop: the orb is drawn once, at rest.
 */
export function VoiceOrb({
  phase,
  tap,
}: Pick<ConversationView, "phase"> & { tap?: AnswerTap | null }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /* Read by the frame loop, which outlives every render; a prop captured by its closure would freeze
     the orb at whatever it heard first. */
  const inputRef = useRef({ phase, tap });
  inputRef.current = { phase, tap };
  /* The effect below owns the canvas; the phase effect after it reaches the still frame through here. */
  const stillRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    // No `matchMedia` means no layout (jsdom), and with no layout there is nothing to draw on.
    if (canvas === null || typeof window.matchMedia !== "function") return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
    const state: Frame = {
      flow: 0,
      wobble: 0,
      energy: 0,
      voice: 0,
      blob: 0,
      harm: new Float32Array(HARMONICS),
      bands: new Float32Array(ANSWER_BANDS),
    };
    let last = performance.now();
    let frame = 0;

    const draw = (now: number) => {
      const { phase, tap } = inputRef.current;
      // Nobody is speaking in `off` and `thinking`, so 30fps reads the same, and a hidden window shows nothing.
      if (document.hidden || (IDLE_PHASES.has(phase) && now - last < IDLE_FRAME_MS)) {
        frame = requestAnimationFrame(draw);
        return;
      }
      const dt = Math.min((now - last) / 1000, 0.1);
      last = now;
      const source = tap ?? SILENT_TAP;
      const speaking = phase === "speaking";
      const d = orbDrive(phase, speaking ? source.level() : 0);
      // Eased, so a level that arrives in steps reads as a swell rather than a twitch.
      // Nobody speaking is not nobody home: the shell keeps a slow breath, while the dark centre waits.
      const target = speaking ? d.energy : Math.max(d.energy, orbBreath(now / 1000));
      state.energy += (target - state.energy) * Math.min(dt * 8, 1);
      state.voice += (d.core - state.voice) * Math.min(dt * 25, 1);
      if (speaking) source.bands(state.bands);
      else state.bands.fill(0);
      state.flow += dt * FLOW[phase];
      state.wobble += dt * (BASE.squiggleSpeed * (phase === "hearing" ? 0.4 : 1) + 4 * state.energy);
      paint(ctx, canvas, state, DIM[phase], dt);
      frame = requestAnimationFrame(draw);
    };

    // A still orb has no loop and nothing to follow: the answer's sound is nil, so it is drawn at rest.
    const drawStill = () => {
      const { phase } = inputRef.current;
      state.flow = 0;
      state.wobble = 0;
      state.energy = 0;
      state.voice = 0;
      state.bands.fill(0);
      // A step of one second settles the outline's smoothing onto its target, which is zero.
      paint(ctx, canvas, state, DIM[phase], 1);
    };
    stillRef.current = drawStill;

    const resize = () => {
      const box = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(box.width * dpr);
      canvas.height = Math.round(box.height * dpr);
      if (reduced.matches) drawStill();
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    // A preference changed while the page is open is obeyed: the loop stops, or starts again.
    const onPreference = () => {
      cancelAnimationFrame(frame);
      if (reduced.matches) {
        drawStill();
      } else {
        last = performance.now();
        frame = requestAnimationFrame(draw);
      }
    };
    reduced.addEventListener("change", onPreference);
    if (!reduced.matches) frame = requestAnimationFrame(draw);

    return () => {
      stillRef.current = null;
      cancelAnimationFrame(frame);
      observer.disconnect();
      reduced.removeEventListener("change", onPreference);
    };
  }, []);

  // Under reduced motion there is no loop, so a new phase is drawn once, when it arrives.
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    if (!window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    stillRef.current?.();
  }, [phase]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden="true"
      className="voice-orb"
      data-hearing={phase === "hearing" ? "" : undefined}
      data-phase={phase}
    />
  );
}

/** Phases where the orb only breathes: its loop runs at a lower rate (about 30fps). */
const IDLE_PHASES: ReadonlySet<string> = new Set(["off", "thinking"]);
const IDLE_FRAME_MS = 30;

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

const TAU = Math.PI * 2;
/** How many low harmonics shape the outline round the dark ball, each fed by a slice of the spectrum. */
const HARMONICS = 5;
/** The outline's points: enough that five low harmonics read as a smooth blob. */
const BLOB_POINTS = 144;
/** The dark ball's colour where the theme has no `--surface-sunken`; jsdom answers "" for every property. */
const BALL_DARK = "#08090c";
const BALL_LIGHT = "#eceef3";

/** What one orb carries from frame to frame. */
interface Frame {
  flow: number;
  wobble: number;
  /** The shell's energy: how hard it squiggles. */
  energy: number;
  /** The dark ball's voice: its size and the outline's rise. */
  voice: number;
  /** The outline's own smoothed level. */
  blob: number;
  /** The five harmonics' smoothed sizes. */
  harm: Float32Array;
  /** The answer's spectrum, low to high. */
  bands: Float32Array;
}

/** `#rrggbb` as an rgba() with the given alpha; any other spelling is returned unchanged. */
function hexA(color: string, alpha: number): string {
  const match = /^#([0-9a-f]{6})$/i.exec(color);
  if (match === null) return color;
  const n = parseInt(match[1]!, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

/** A colour as the browser spells it (`#rrggbb` or `rgb()`/`rgba()`) with the given alpha. */
function rgbaOf(color: string, alpha: number): string {
  const a = Math.max(0, Math.min(1, alpha));
  if (color.startsWith("#")) return hexA(color, a);
  const match = /rgba?\(([^)]+)\)/.exec(color);
  if (match === null) return color;
  const [r, g, b] = match[1]!.split(",").map((x) => parseFloat(x));
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}

function paint(ctx: CanvasRenderingContext2D, canvas: HTMLCanvasElement, state: Frame, dim: number, dt: number) {
  const { flow, wobble, energy, harm, bands } = state;
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  if (width === 0 || height === 0) return;

  const dpr = window.devicePixelRatio || 1;
  const scale = (Math.min(width, height) / 2) * 0.78 / BASE.radius;
  const cx = width / 2;
  const cy = height / 2;
  const amount = BASE.squiggleAmount + 0.08 * energy;
  const freq = BASE.squiggleFrequency;
  const L = Math.min(state.voice, 1);
  const brightness = dim * (0.8 + 0.2 * Math.min(energy, 1));

  // The colour is the canvas's own CSS `color`, so the theme and the hearing colour — and the
  // transition between them — come from the stylesheet rather than from a literal here.
  // One style read per frame: the colour is animated by a CSS transition, so it cannot be cached.
  const style = getComputedStyle(canvas);
  const shellColor = style.color;
  const back =
    style.getPropertyValue("--surface-sunken").trim() ||
    (window.matchMedia("(prefers-color-scheme: light)").matches ? BALL_LIGHT : BALL_DARK);
  const lw = BASE.lineWidth * dpr * Math.max(0.8, Math.min(1.4, width / (300 * dpr)));
  ctx.strokeStyle = shellColor;
  ctx.lineWidth = lw;
  // Round on purpose: where a line crosses from one opacity band to the next, the two caps overlap
  // and leave a faint bead. Butt caps remove it, and were tried; the beads are the look we kept.
  ctx.lineCap = "round";
  ctx.lineJoin = "round";

  // The dark ball at the heart, with an outline behind it: hidden in silence, it rises round the edge as
  // the answer plays. It is a smooth blob and not a bar chart — a few low harmonics, each one's size
  // taken from a slice of the spectrum.
  const rBall = BASE.radius * scale * 0.25 * (1 + 0.16 * L);
  for (let k = 0; k < HARMONICS; k++) {
    let sum = 0;
    const from = Math.floor((k / HARMONICS) * ANSWER_BANDS);
    const to = Math.floor(((k + 1) / HARMONICS) * ANSWER_BANDS);
    for (let b = from; b < to; b++) sum += bands[b]!;
    const target = sum / (to - from);
    harm[k] = harm[k]! + (target - harm[k]!) * Math.min(dt * (target > harm[k]! ? 10 : 4), 1);
  }
  state.blob += (L - state.blob) * Math.min(dt * 8, 1);
  const blob = state.blob;
  ctx.beginPath();
  for (let i = 0; i <= BLOB_POINTS; i++) {
    const a = (i / BLOB_POINTS) * TAU;
    let wave = 0;
    for (let k = 0; k < HARMONICS; k++) wave += harm[k]! * Math.cos((k + 2) * a + wobble * (0.35 + 0.12 * k) + k * 1.7);
    const r = rBall * (0.97 + 0.42 * blob + 0.38 * wave);
    const x = cx + Math.sin(a) * r;
    const y = cy - Math.cos(a) * r;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.closePath();
  ctx.fillStyle = rgbaOf(shellColor, (0.04 + 0.1 * blob) * dim);
  ctx.fill();
  ctx.lineWidth = lw * 0.75;
  ctx.strokeStyle = rgbaOf(shellColor, (0.5 + 0.45 * Math.min(blob * 1.5, 1)) * dim);
  ctx.stroke();
  ctx.fillStyle = back;
  ctx.beginPath();
  ctx.arc(cx, cy, rBall, 0, TAU);
  ctx.fill();
  ctx.lineWidth = 1 * dpr;
  ctx.strokeStyle = rgbaOf(shellColor, (0.2 + 0.35 * L) * dim);
  ctx.stroke();
  ctx.strokeStyle = shellColor;
  ctx.lineWidth = lw;
  const rB2 = rBall * rBall;

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
        // The far half of the shell passes behind the dark ball, so it is hidden there.
        if (a < 0.575) {
          const mx = (xs[i]! + xs[i + 1]!) / 2 - cx;
          const my = (ys[i]! + ys[i + 1]!) / 2 - cy;
          if (mx * mx + my * my < rB2) {
            open = false;
            continue;
          }
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
