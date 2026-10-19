import { useEffect, useRef } from "react";
import { SILENT_TAP, type AnswerTap } from "../data/answer-tap";
import type { ConversationPhase } from "../lib/conversation";

/**
 * The voice room's stage: a Chladni plate drawn in sand.
 *
 * A round plate sits under the orb with fine sand on it, inside a larger square sheet of even finer
 * sand that fills the rest of the room. Each is shaken by a standing-wave figure, and the figure
 * changes with the conversation's phase, so the sand drifts from one drawing to the next as the turn
 * moves from listening to hearing to thinking to speaking. How hard it is shaken is the room's energy:
 * the user's microphone while a turn can open, a fraction of the answer's loudness while it plays.
 *
 * Everything that decides how it moves is a pure function exported from this module, so the tests pin
 * those directly; only the paint needs a canvas. Like `VoiceOrb` it is decoration and says so to
 * assistive technology, and under reduced motion it runs no loop at all — the sand is settled for the
 * current phase and drawn once.
 *
 * Ported from the prototype's variant G7 (a round plate in a sheet of G4's fine sand).
 */

/** The prototype's `chaos`: how often sand jumps on its own, how freely it moves, how much is loose. */
export const CHAOS = 0.21;

/** How fast the orb's lines flow, per phase. Shared with the orb that stands in the middle of this room. */
export const FLOW: Record<ConversationPhase, number> = {
  off: 0.5,
  listening: 0.8,
  hearing: 0.3,
  thinking: 2.5,
  speaking: 1.3,
};

/** How bright the orb is, per phase. */
export const DIM: Record<ConversationPhase, number> = {
  off: 0.6,
  listening: 0.85,
  hearing: 0.62,
  thinking: 0.9,
  speaking: 1,
};

/** CSS pixels the top bar takes at the top of the room. Mirrored by `.voice-orb` in voice.css. */
export const ROOM_TOP = 112;
/** CSS pixels the dock takes at the bottom of the room, at the least: a taller dock is measured and passed in. */
export const ROOM_BOTTOM = 96;

/** The smallest the orb gets, in CSS pixels. Mirrored by `--voice-orb-size` in voice.css. It was 140, and
 *  a short window with a stacked dock then had the orb's lower edge under the captions. */
export const ORB_FLOOR = 96;

/**
 * Where the orb stands in the room, how big its shell is, and how wide the plate is. All in CSS pixels.
 * The orb and its plate are centred in the free band between the top bar and the dock, and sized to fit
 * it: the plate never runs under either bar (the floors only bite in a room too small to fit anything).
 * `bottom` is the dock's measured height, the same number voice.css reads as `--voice-dock-h`: with the
 * constant here instead, a narrow window's stacked dock had the plate drawn under it and the orb (placed
 * by the stylesheet from the measured height) standing off the plate's hole.
 */
export function orbGeometry(
  w: number,
  h: number,
  bottom: number = ROOM_BOTTOM,
): { cx: number; cy: number; R: number; plate: number } {
  const free = Math.max(h - ROOM_TOP - bottom, 0);
  const orbSize = Math.max(ORB_FLOOR, Math.min(free * 0.44, w * 0.5));
  const R = (orbSize / 2) * 0.78;
  return {
    cx: w / 2,
    cy: ROOM_TOP + free / 2,
    R,
    plate: Math.max(R * 1.15, Math.min(R * 2.6, free / 2 - 8, w * 0.47)),
  };
}

/** The room's slow breath when nobody is speaking, as a share of full shaking. */
export const AMBIENT = 0.1;

/** The ambient energy at time `t` (seconds): a slow swell around `AMBIENT`, within [0.04, 0.16]. */
export function ambientEnergy(t: number): number {
  return AMBIENT * (1 + 0.6 * Math.sin(t * 0.5));
}

/** The orb shell's own slow breath at time `t` (seconds), within [0.02, 0.08]. The dark centre gets none. */
export function orbBreath(t: number): number {
  return 0.02 + 0.03 * (1 + Math.sin(t * 0.9));
}

/** The microphone, against the bar that would open a turn — and only while a turn can open. */
export function micTarget(phase: ConversationPhase, level: number, threshold: number): number {
  return phase === "listening" || phase === "hearing" ? Math.min(level / Math.max(threshold, 0.01), 1.5) : 0;
}

/**
 * What shakes the sand: the microphone, except while the answer plays. Then the microphone is open and
 * hearing the speakers, so the sand follows the orb's own energy instead, at 0.4 of it.
 */
export function roomEnergy(phase: ConversationPhase, mic: number, orbEnergy: number): number {
  return phase === "speaking" ? orbEnergy * 0.4 : mic;
}

/** The orb is nucleos's voice: it holds still while the user talks and moves with the answer. */
export function orbDrive(phase: ConversationPhase, voice: number): { energy: number; core: number } {
  const speaking = phase === "speaking";
  const thinking = phase === "thinking";
  return {
    energy: speaking ? voice * 1.4 : thinking ? 0.15 : 0,
    core: speaking ? voice * 1.2 : thinking ? 0.06 : 0,
  };
}

/** A standing-wave figure over [-1, 1]²: where it is zero the sand collects. */
export type Field = (u: number, v: number) => number;

/** A square plate's figure, from two mode numbers. */
export const sqField = (u: number, v: number, n: number, m: number): number =>
  Math.cos(n * Math.PI * u) * Math.cos(m * Math.PI * v) - Math.cos(m * Math.PI * u) * Math.cos(n * Math.PI * v);

/** A round plate's figure: `m` nodal diameters and `k` nodal circles. */
export const discField = (u: number, v: number, m: number, k: number): number => {
  const r = Math.hypot(u, v);
  const t = Math.atan2(v, u);
  return Math.cos(m * t) * Math.cos((k + 0.5) * Math.PI * r);
};

type Modes = readonly [number, number];

/** The square sheet's figure per phase. */
export const MODES_SQ: Record<ConversationPhase, Modes> = {
  off: [2, 3],
  listening: [1, 4],
  hearing: [3, 7],
  thinking: [2, 5],
  speaking: [4, 7],
};

/** The round plate's figure per phase. */
export const MODES_DISC: Record<ConversationPhase, Modes> = {
  off: [2, 1],
  listening: [3, 1],
  hearing: [5, 2],
  thinking: [2, 2],
  speaking: [6, 3],
};

/** How one sheet of sand behaves. */
export interface Variant {
  grains: number;
  shape: "disc" | "square";
  /** The shaking that is always there. */
  base: number;
  /** The shaking that the room's energy adds. */
  shake: number;
  /** How hard a grain is pulled toward a line where the figure is zero. */
  pull: number;
  /** Grain size in CSS pixels. */
  size: number;
  alpha: number;
}

/** The fine square sheet that fills the room. */
export const G4: Variant = { grains: 22000, shape: "square", base: 0.0012, shake: 0.007, pull: 0.0006, size: 0.9, alpha: 0.34 };

/** The round plate under the orb. */
export const G7: Variant = { grains: 9000, shape: "disc", base: 0.003, shake: 0.012, pull: 0.0012, size: 1.2, alpha: 0.7 };

const MAX_GRAINS = 22000;
const TAU = Math.PI * 2;
const lerp = (a: number, b: number, t: number): number => a + (b - a) * t;
/** Three brightnesses the grains are drawn in, so the sand has depth without a fill style per grain. */
const SHADES = [0.45, 0.75, 1];

/** Everything a room's sand is made of. Positions are in plate units, [-1, 1]. */
export interface Sand {
  /** The round plate's grains, x then y. */
  sand: Float32Array;
  vel: Float32Array;
  /** The square sheet's grains. */
  osand: Float32Array;
  ovel: Float32Array;
  /** How freely each grain moves. */
  mob: Float32Array;
  /** How big each grain is, as a factor. */
  gsz: Float32Array;
  /** Below `CHAOS * 0.22` a grain ignores the nodal lines altogether. */
  freeKey: Float32Array;
  /** The room's own random numbers in (0, 1): a seeded LCG, so the same seed draws the same sand. */
  rnd: () => number;
}

/** A room's sand, made from a seed. Every grain has its own character, so the sand never settles into one clean drawing. */
export function createSand(seed = 11): Sand {
  const modulus = 2147483647;
  let state = Math.floor(Math.abs(seed)) % modulus;
  if (state === 0) state = 1;
  const rnd = () => {
    state = (state * 16807) % modulus;
    return state / modulus;
  };

  const sand = new Float32Array(MAX_GRAINS * 2);
  const osand = new Float32Array(MAX_GRAINS * 2);
  const mob = new Float32Array(MAX_GRAINS);
  const gsz = new Float32Array(MAX_GRAINS);
  const freeKey = new Float32Array(MAX_GRAINS);
  for (let i = 0; i < MAX_GRAINS; i++) {
    const r = rnd();
    mob[i] = 0.35 + 1.6 * r * r;
    gsz[i] = 0.6 + 1.6 * Math.pow(rnd(), 2.5);
    freeKey[i] = rnd();
  }
  // The round plate's sand starts spread evenly over the disc, the sheet's anywhere in the square.
  for (let i = 0; i < G7.grains; i++) {
    const r = Math.sqrt(rnd());
    const t = rnd() * TAU;
    sand[2 * i] = Math.cos(t) * r;
    sand[2 * i + 1] = Math.sin(t) * r;
  }
  for (let i = 0; i < G4.grains * 2; i++) osand[i] = rnd() * 2 - 1;

  return {
    sand,
    vel: new Float32Array(MAX_GRAINS * 2),
    osand,
    ovel: new Float32Array(MAX_GRAINS * 2),
    mob,
    gsz,
    freeKey,
    rnd,
  };
}

interface Kick {
  x: number;
  y: number;
  radius: number;
  strength: number;
}

/**
 * One step of one sheet of sand. A grain is pulled down the figure's gradient toward the line where it
 * is zero, shaken at random by an amount that grows with the energy, and now and then a whole patch of
 * sand jumps. `active` false is a stopped room: what was already moving coasts to a stop, and
 * nothing new starts.
 */
export function stepSand(
  V: Variant,
  F: Field,
  active: boolean,
  energy: number,
  dt: number,
  s: Sand,
  arr: Float32Array,
  va: Float32Array,
): void {
  const rnd01 = s.rnd;
  const rand = () => rnd01() - 0.5;
  const n2 = V.grains * 2;
  if (!active) {
    for (let i = 0; i < n2; i++) {
      va[i] = va[i]! * 0.8;
      arr[i] = arr[i]! + va[i]!;
    }
    for (let i = 0; i < n2; i++) arr[i] = Math.max(-1, Math.min(1, arr[i]!));
    return;
  }

  const e = 0.004;
  const loud = Math.min(energy, 1.3);
  const shake = V.base + V.shake * loud;
  const floor = CHAOS * 0.005;
  const freeShare = CHAOS * 0.22;
  // Random kicks: a patch of sand jumps, more often the louder it gets.
  const kicks: Kick[] = [];
  let expected = CHAOS * (1.2 + 7 * loud) * dt;
  while (rnd01() < expected) {
    kicks.push({
      x: rnd01() * 2 - 1,
      y: rnd01() * 2 - 1,
      radius: 0.06 + rnd01() * 0.22,
      strength: (0.003 + rnd01() * 0.01) * (0.4 + loud),
    });
    expected -= 1;
  }

  for (let i = 0; i < V.grains; i++) {
    let u = arr[2 * i]!;
    let vv = arr[2 * i + 1]!;
    const m = s.mob[i]!;
    const loose = s.freeKey[i]! < freeShare;
    const f = F(u, vv);
    let ax = 0;
    let ay = 0;
    if (!loose) {
      const fu = (F(u + e, vv) - f) / e;
      const fv = (F(u, vv + e) - f) / e;
      ax = -f * fu * V.pull * m;
      ay = -f * fv * V.pull * m;
    }
    const amp = (shake * (0.25 + Math.abs(f)) + floor) * m * (loose ? 1.7 : 1);
    ax += rand() * amp;
    ay += rand() * amp;
    for (let k = 0; k < kicks.length; k++) {
      const kick = kicks[k]!;
      const dx = u - kick.x;
      const dy = vv - kick.y;
      const d2 = dx * dx + dy * dy;
      if (d2 >= kick.radius * kick.radius) continue;
      const d = Math.sqrt(d2) + 1e-4;
      const push = kick.strength * (1 - d / kick.radius) * m;
      va[2 * i] = va[2 * i]! + (dx / d) * push + rand() * push;
      va[2 * i + 1] = va[2 * i + 1]! + (dy / d) * push + rand() * push;
    }
    va[2 * i] = va[2 * i]! * 0.86;
    va[2 * i + 1] = va[2 * i + 1]! * 0.86;
    u += ax + va[2 * i]!;
    vv += ay + va[2 * i + 1]!;
    if (V.shape === "disc") {
      const r = Math.hypot(u, vv);
      if (r > 1) {
        u /= r;
        vv /= r;
        va[2 * i] = va[2 * i]! * -0.5;
        va[2 * i + 1] = va[2 * i + 1]! * -0.5;
      }
    } else {
      if (u < -1 || u > 1) {
        u = u < -1 ? -1 : 1;
        va[2 * i] = va[2 * i]! * -0.5;
      }
      if (vv < -1 || vv > 1) {
        vv = vv < -1 ? -1 : 1;
        va[2 * i + 1] = va[2 * i + 1]! * -0.5;
      }
    }
    arr[2 * i] = u;
    arr[2 * i + 1] = vv;
  }
}

/** A sheet's figure while it moves from one phase's modes to the next's. */
interface ModeTrack {
  from: Modes;
  to: Modes;
  mix: number;
  phase: ConversationPhase | null;
}

const newTrack = (table: Record<ConversationPhase, Modes>): ModeTrack => ({
  from: table.off,
  to: table.off,
  mix: 1,
  phase: null,
});

/** Starts the blend toward `phase`'s modes when the phase has changed; the very first call starts at rest on them. */
function trackMode(track: ModeTrack, table: Record<ConversationPhase, Modes>, phase: ConversationPhase): void {
  if (track.phase === null) {
    track.from = table[phase];
    track.to = table[phase];
    track.mix = 1;
    track.phase = phase;
  } else if (track.phase !== phase) {
    track.phase = phase;
    track.from = track.to;
    track.to = table[phase];
    track.mix = 0;
  }
}

/** The figure a track is at now: a blend of the old modes and the new. */
function blended(track: ModeTrack, field: (u: number, v: number, a: number, b: number) => number): Field {
  const [a1, b1] = track.to;
  if (track.mix >= 1) return (u, v) => field(u, v, a1, b1);
  const [a0, b0] = track.from;
  const mix = track.mix;
  return (u, v) => lerp(field(u, v, a0, b0), field(u, v, a1, b1), mix);
}

/** The room's colours, read from the theme so the sand follows light and dark. */
interface Palette {
  muted: string;
  border: string;
  borderStrong: string;
  back: string;
  plate: string;
  bg: string;
  light: boolean;
}

/** jsdom answers "" for every custom property, and a theme may lack one: a missing colour is a fallback, never a throw. */
function readPalette(light: boolean): Palette {
  const css = getComputedStyle(document.documentElement);
  const pick = (name: string, dark: string, bright: string) => css.getPropertyValue(name).trim() || (light ? bright : dark);
  return {
    muted: pick("--text-muted", "#98a1b2", "#545d6c"),
    border: pick("--border", "#262b35", "#dcdfe6"),
    borderStrong: pick("--border-strong", "#39404d", "#b4bbc7"),
    back: pick("--surface-sunken", "#08090c", "#eceef3"),
    plate: pick("--bg", "#0c0e12", "#f5f6f9"),
    bg: pick("--bg", "#0c0e12", "#f5f6f9"),
    light,
  };
}

/** `#rrggbb` as an rgba() with the given alpha; any other spelling is returned unchanged. */
function withAlpha(color: string, alpha: number): string {
  const match = /^#([0-9a-f]{6})$/i.exec(color);
  if (match === null) return color;
  const n = parseInt(match[1]!, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

/** The room's box, in CSS pixels, and the pixel ratio the canvas was sized for. */
interface Box {
  w: number;
  h: number;
  dpr: number;
  /** The dock's height, as `orbGeometry` takes it. */
  bottom: number;
}

/** A darkening toward the room's corners, so the sheet's edge does not read as the end of the world. */
function vignette(ctx: CanvasRenderingContext2D, box: Box, palette: Palette, inner: number, strength: number): void {
  const { w, h } = box;
  const { cx, cy } = orbGeometry(w, h, box.bottom);
  const vig = ctx.createRadialGradient(cx, cy, inner, cx, cy, Math.max(w, h) * 0.72);
  vig.addColorStop(0, withAlpha(palette.bg, 0));
  vig.addColorStop(1, withAlpha(palette.bg, palette.light ? strength * 0.25 : strength));
  ctx.fillStyle = vig;
  ctx.fillRect(0, 0, w, h);
}

/** One frame of the room: the sheet, the round plate, and the hole where the orb stands. */
function drawRoom(ctx: CanvasRenderingContext2D, box: Box, palette: Palette, s: Sand): void {
  const { w, h, dpr } = box;
  if (w === 0 || h === 0) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const { cx, cy, R, plate: Sd } = orbGeometry(w, h, box.bottom);

  ctx.fillStyle = palette.back;
  ctx.fillRect(0, 0, w, h);
  const S4 = Math.max(w, h) * 0.52;

  // Outside the big circle: the fine sheet as it is, with nothing drawn under the plate.
  const edge2 = (Sd + 8) * (Sd + 8);
  ctx.fillStyle = palette.muted;
  const a4 = palette.light ? Math.min(1, G4.alpha + 0.2) : G4.alpha;
  for (let g = 0; g < 3; g++) {
    ctx.globalAlpha = a4 * SHADES[g]!;
    for (let i = g; i < G4.grains; i += 3) {
      const x = cx + s.osand[2 * i]! * S4;
      const y = cy + s.osand[2 * i + 1]! * S4;
      if (x < -2 || x > w + 2 || y < -2 || y > h + 2) continue;
      const dx = x - cx;
      const dy = y - cy;
      if (dx * dx + dy * dy < edge2) continue;
      const sz = G4.size * s.gsz[i]!;
      ctx.fillRect(x, y, sz, sz);
    }
  }
  ctx.globalAlpha = 1;

  // The round plate, with its outline and a second ring outside it.
  ctx.fillStyle = palette.plate;
  ctx.beginPath();
  ctx.arc(cx, cy, Sd, 0, TAU);
  ctx.fill();
  ctx.strokeStyle = palette.borderStrong;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.strokeStyle = palette.border;
  ctx.beginPath();
  ctx.arc(cx, cy, Sd + 6, 0, TAU);
  ctx.stroke();

  // Its sand, except under the ball.
  const hole2 = R * 1.04 * (R * 1.04);
  ctx.fillStyle = palette.muted;
  const ad = palette.light ? Math.min(1, G7.alpha + 0.2) : G7.alpha;
  for (let g = 0; g < 3; g++) {
    ctx.globalAlpha = ad * SHADES[g]!;
    for (let i = g; i < G7.grains; i += 3) {
      const x = cx + s.sand[2 * i]! * Sd;
      const y = cy + s.sand[2 * i + 1]! * Sd;
      const dx = x - cx;
      const dy = y - cy;
      if (dx * dx + dy * dy < hole2) continue;
      const sz = G7.size * s.gsz[i]!;
      ctx.fillRect(x, y, sz, sz);
    }
  }
  ctx.globalAlpha = 1;

  // No ground where the ball's shell is.
  ctx.fillStyle = palette.back;
  ctx.beginPath();
  ctx.arc(cx, cy, R * 1.04, 0, TAU);
  ctx.fill();
  vignette(ctx, box, palette, Sd * 0.9, 0.12);
}

/** How many steps a still room runs to settle into the figure of its phase. */
const SETTLE_STEPS = 90;
const SETTLE_DT = 1 / 60;

export function VoiceRoom({
  phase,
  level,
  threshold,
  tap,
  dock = ROOM_BOTTOM,
}: {
  phase: ConversationPhase;
  level: number;
  threshold: number;
  tap?: AnswerTap | null;
  /** The dock's measured height in CSS pixels; the plate is centred in the band above it. */
  dock?: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  /* Read by the frame loop, which outlives every render; a prop captured by its closure would freeze
     the room at whatever it heard first. */
  const inputRef = useRef({ phase, level, threshold, tap, dock });
  inputRef.current = { phase, level, threshold, tap, dock };
  /* The effect below owns the canvas and the sand; the phase effect after it reaches `settle` through here. */
  const settleRef = useRef<((force: boolean) => void) | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    // No `matchMedia` means no layout (jsdom), and with no layout there is nothing to draw on.
    if (canvas === null || typeof window.matchMedia !== "function") return;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
    const scheme = window.matchMedia("(prefers-color-scheme: light)");
    // An explicit `data-theme` on the root beats the system preference, as it does in the stylesheets.
    const isLight = () => {
      const forced = document.documentElement.getAttribute("data-theme");
      return forced === "light" ? true : forced === "dark" ? false : scheme.matches;
    };
    let palette = readPalette(isLight());
    const box: Box = { w: 0, h: 0, dpr: 1, bottom: inputRef.current.dock };
    // Every frame takes the dock's height as it is now: it grows and shrinks without the canvas resizing.
    const paint = () => {
      box.bottom = inputRef.current.dock;
      drawRoom(ctx, box, palette, sand);
    };
    const sand = createSand();
    const plate = newTrack(MODES_DISC);
    const outer = newTrack(MODES_SQ);
    const anim = { mic: 0, orbE: 0, last: performance.now() };
    let settledFor: ConversationPhase | null = null;
    let frame = 0;

    const step = (active: boolean, energy: number, dt: number, fd: Field, fs: Field) => {
      stepSand(G4, fs, active, energy, dt, sand, sand.osand, sand.ovel);
      stepSand(G7, fd, active, energy, dt, sand, sand.sand, sand.vel);
    };

    const loop = (now: number) => {
      const { phase, level, threshold, tap } = inputRef.current;
      // The sand is the page's heaviest loop (about 31 000 grains a frame). With nobody speaking it
      // runs at about 30fps, and a hidden window skips it altogether.
      if (document.hidden || ((phase === "off" || phase === "thinking") && now - anim.last < 30)) {
        frame = requestAnimationFrame(loop);
        return;
      }
      const dt = Math.min((now - anim.last) / 1000, 0.1);
      anim.last = now;
      // The room hears you: your microphone, measured against the bar that opens a turn.
      anim.mic += (micTarget(phase, level, threshold) - anim.mic) * Math.min(dt * 6, 1);
      // The orb is nucleos's voice: it moves when nucleos speaks, and holds still to listen.
      const voice = phase === "speaking" ? (tap ?? SILENT_TAP).level() : 0;
      anim.orbE += (orbDrive(phase, voice).energy - anim.orbE) * Math.min(dt * 8, 1);
      // Never frozen: with nobody speaking the room still breathes slowly, so the sand keeps forming its figure.
      const energy = Math.max(roomEnergy(phase, anim.mic, anim.orbE), ambientEnergy(now / 1000));

      trackMode(plate, MODES_DISC, phase);
      plate.mix = Math.min(1, plate.mix + dt * 0.8);
      trackMode(outer, MODES_SQ, phase);
      outer.mix = Math.min(1, outer.mix + dt * 0.8);
      step(true, energy, dt, blended(plate, discField), blended(outer, sqField));
      paint();
      frame = requestAnimationFrame(loop);
    };

    // Runs the sand to rest on a phase's figure and points both tracks at it, so a later start blends from here.
    const rest = (phase: ConversationPhase) => {
      const [m, k] = MODES_DISC[phase];
      const [a, b] = MODES_SQ[phase];
      const fd: Field = (u, v) => discField(u, v, m, k);
      const fs: Field = (u, v) => sqField(u, v, a, b);
      for (let i = 0; i < SETTLE_STEPS; i++) step(true, 0.5, SETTLE_DT, fd, fs);
      // The figures are at rest on this phase, so a later start from them blends from here.
      plate.phase = phase;
      plate.from = MODES_DISC[phase];
      plate.to = MODES_DISC[phase];
      plate.mix = 1;
      outer.phase = phase;
      outer.from = MODES_SQ[phase];
      outer.to = MODES_SQ[phase];
      outer.mix = 1;
    };

    // A still room has no loop: it settles on the current phase's figure and paints that.
    const settle = (force: boolean) => {
      if (!reduced.matches) return;
      const { phase } = inputRef.current;
      if (!force && settledFor === phase) return;
      settledFor = phase;
      rest(phase);
      paint();
    };
    settleRef.current = settle;

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      box.w = rect.width;
      box.h = rect.height;
      box.dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(rect.width * box.dpr);
      canvas.height = Math.round(rect.height * box.dpr);
      if (reduced.matches) settle(true);
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    // A preference changed while the page is open is obeyed: the loop stops, or starts again.
    const onMotion = () => {
      cancelAnimationFrame(frame);
      if (reduced.matches) {
        settle(true);
      } else {
        anim.last = performance.now();
        frame = requestAnimationFrame(loop);
      }
    };
    // The theme changed: take its colours again, and repaint at once if nothing is going to.
    const onScheme = () => {
      palette = readPalette(isLight());
      if (reduced.matches) paint();
    };
    // A theme picked inside the app is an attribute on the root, which `prefers-color-scheme` never reports.
    const themeObserver =
      typeof MutationObserver === "function" ? new MutationObserver(onScheme) : null;
    themeObserver?.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    reduced.addEventListener("change", onMotion);
    scheme.addEventListener("change", onScheme);
    // Moving or still, the first frame already shows the phase's figure instead of random dust.
    if (!reduced.matches) {
      rest(inputRef.current.phase);
      frame = requestAnimationFrame(loop);
    }

    return () => {
      settleRef.current = null;
      cancelAnimationFrame(frame);
      observer.disconnect();
      themeObserver?.disconnect();
      reduced.removeEventListener("change", onMotion);
      scheme.removeEventListener("change", onScheme);
    };
  }, []);

  // Under reduced motion there is no loop, so a new phase settles the sand once, when it arrives.
  useEffect(() => {
    settleRef.current?.(false);
  }, [phase]);

  // Nor is there a frame to pick up a dock that changed height: repaint the still room around it.
  useEffect(() => {
    settleRef.current?.(true);
  }, [dock]);

  return <canvas ref={canvasRef} aria-hidden="true" className="voice-room-canvas" />;
}
