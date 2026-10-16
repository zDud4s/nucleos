import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";
import {
  AMBIENT,
  CHAOS,
  DIM,
  FLOW,
  G4,
  G7,
  MODES_DISC,
  MODES_SQ,
  ROOM_BOTTOM,
  ROOM_TOP,
  VoiceRoom,
  ambientEnergy,
  createSand,
  discField,
  micTarget,
  orbBreath,
  orbDrive,
  orbGeometry,
  roomEnergy,
  sqField,
  stepSand,
} from "./VoiceRoom";
import { VoiceOrb } from "./VoiceOrb";

/* The room is a Chladni plate drawn from the voice conversation's phase. Everything that decides how
   it moves is a pure function exported from the module, so the cases below pin those directly and
   leave only the reduced-motion paint — which needs a canvas — to a render. */

const FRAME = 1 / 60;

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("the room's drive", () => {
  it("the room hears the microphone only while listening, and the voice while answering", () => {
    // The microphone is measured against the bar that would open a turn, and only while a turn can open.
    expect(micTarget("listening", 0.3, 0.6)).toBeCloseTo(0.5, 6);
    expect(micTarget("hearing", 0.3, 0.6)).toBeCloseTo(0.5, 6);
    expect(micTarget("hearing", 0.9, 0.6)).toBeCloseTo(1.5, 6);
    // Capped at one and a half bars, and the bar itself is floored so a zero threshold cannot divide.
    expect(micTarget("listening", 5, 0.6)).toBe(1.5);
    expect(micTarget("listening", 1, 0)).toBe(1.5);
    // Anywhere else the microphone says nothing, however loud the room is.
    expect(micTarget("off", 1, 0.5)).toBe(0);
    expect(micTarget("thinking", 1, 0.5)).toBe(0);
    expect(micTarget("speaking", 1, 0.5)).toBe(0);

    // The sand follows the microphone, except while the answer plays: then it follows the orb's energy,
    // at 0.4 of it, because the microphone is open and is hearing the speakers.
    expect(roomEnergy("listening", 0.7, 5)).toBe(0.7);
    expect(roomEnergy("hearing", 0.7, 5)).toBe(0.7);
    expect(roomEnergy("thinking", 0.7, 5)).toBe(0.7);
    expect(roomEnergy("off", 0.7, 5)).toBe(0.7);
    expect(roomEnergy("speaking", 0.7, 2)).toBeCloseTo(0.8, 6);
  });

  it("the orb holds still while the user talks and moves with the answer", () => {
    for (const phase of ["off", "listening", "hearing"] as const) {
      expect(orbDrive(phase, 0.8)).toEqual({ energy: 0, core: 0 });
    }
    // Thinking is a low constant stir that ignores the voice entirely.
    expect(orbDrive("thinking", 0.8)).toEqual({ energy: 0.15, core: 0.06 });
    expect(orbDrive("thinking", 0)).toEqual({ energy: 0.15, core: 0.06 });
    // Speaking is the answer's level, scaled.
    const speaking = orbDrive("speaking", 0.5);
    expect(speaking.energy).toBeCloseTo(0.7, 6);
    expect(speaking.core).toBeCloseTo(0.6, 6);
    expect(orbDrive("speaking", 0)).toEqual({ energy: 0, core: 0 });

    // How fast the lines flow and how bright the orb is, per phase.
    expect(CHAOS).toBe(0.21);
    expect(FLOW).toEqual({ off: 0.5, listening: 0.8, hearing: 0.3, thinking: 2.5, speaking: 1.3 });
    expect(DIM).toEqual({ off: 0.6, listening: 0.85, hearing: 0.62, thinking: 0.9, speaking: 1 });
  });

  it("the orb and its plate fit between the top bar and the dock", () => {
    expect(ROOM_TOP).toBe(112);
    expect(ROOM_BOTTOM).toBe(96);

    // 800x600: 392px are free between the bars; the orb is 44% of that, centred in it, and the plate
    // is 2.6 radii (the free half-height would allow 188, the width 376).
    const wide = orbGeometry(800, 600);
    expect(wide.cx).toBe(400);
    expect(wide.cy).toBeCloseTo(308, 6);
    expect(wide.R).toBeCloseTo(((0.44 * 392) / 2) * 0.78, 6);
    expect(wide.plate).toBeCloseTo(wide.R * 2.6, 6);

    // A tall narrow room is bound by the width, and so is its plate (47% of it).
    const narrow = orbGeometry(300, 900);
    expect(narrow.R).toBeCloseTo((150 / 2) * 0.78, 6);
    expect(narrow.plate).toBeCloseTo(300 * 0.47, 6);

    // The orb never shrinks below 140 whatever the room, and the plate never below 1.15 radii.
    const tiny = orbGeometry(100, 100);
    expect(tiny.cx).toBe(50);
    expect(tiny.cy).toBeCloseTo(112, 6);
    expect(tiny.R).toBeCloseTo((140 / 2) * 0.78, 6);
    expect(tiny.plate).toBeCloseTo(tiny.R * 1.15, 6);

    // On a 1920x1080 window's room the plate clears both bars.
    const full = orbGeometry(1920, 990);
    expect(full.cy - full.plate).toBeGreaterThanOrEqual(ROOM_TOP);
    expect(full.cy + full.plate).toBeLessThanOrEqual(990 - ROOM_BOTTOM);
  });

  it("the room breathes when nobody speaks", () => {
    expect(AMBIENT).toBe(0.1);
    expect(ambientEnergy(0)).toBeCloseTo(0.1, 9);
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = 0; i <= 800; i++) {
      const e = ambientEnergy(i * 0.05);
      expect(e).toBeGreaterThanOrEqual(0.04 - 1e-9);
      expect(e).toBeLessThanOrEqual(0.16 + 1e-9);
      lo = Math.min(lo, e);
      hi = Math.max(hi, e);
    }
    expect(lo).toBeLessThan(0.05);
    expect(hi).toBeGreaterThan(0.15);
  });

  it("the orb breathes, but its centre waits for the voice", () => {
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = 0; i <= 800; i++) {
      const b = orbBreath(i * 0.05);
      expect(b).toBeGreaterThanOrEqual(0.02 - 1e-9);
      expect(b).toBeLessThanOrEqual(0.08 + 1e-9);
      lo = Math.min(lo, b);
      hi = Math.max(hi, b);
    }
    expect(hi).toBeGreaterThan(lo);
    // The breath belongs to the shell alone: the dark centre still waits for nucleos's voice.
    for (const phase of ["off", "listening", "hearing"] as const) {
      expect(orbDrive(phase, 0.8).core).toBe(0);
    }
  });
});

describe("the sand", () => {
  const plateField = (phase: keyof typeof MODES_DISC) => {
    const [m, k] = MODES_DISC[phase];
    return (u: number, v: number) => discField(u, v, m, k);
  };
  const outerField = (phase: keyof typeof MODES_SQ) => {
    const [n, m] = MODES_SQ[phase];
    return (u: number, v: number) => sqField(u, v, n, m);
  };

  it("sand stays on its plate however hard it is shaken", () => {
    expect(G7.grains).toBe(9000);
    expect(G4.grains).toBe(22000);

    const s = createSand();
    expect(s.sand.length).toBe(22000 * 2);
    expect(s.osand.length).toBe(22000 * 2);
    expect(s.mob.length).toBe(22000);

    // The plate's sand starts inside the disc, the outer sheet's anywhere in the square.
    for (let i = 0; i < G7.grains; i++) {
      expect(Math.hypot(s.sand[2 * i]!, s.sand[2 * i + 1]!)).toBeLessThanOrEqual(1 + 1e-6);
    }
    let outerMax = 0;
    for (let i = 0; i < G4.grains * 2; i++) outerMax = Math.max(outerMax, Math.abs(s.osand[i]!));
    expect(outerMax).toBeGreaterThan(0.9);
    expect(outerMax).toBeLessThanOrEqual(1);

    const before = Float32Array.from(s.sand);
    const F = plateField("speaking");
    const outer = outerField("speaking");
    for (let step = 0; step < 200; step++) {
      stepSand(G7, F, true, 1.3, FRAME, s, s.sand, s.vel);
      stepSand(G4, outer, true, 1.3, FRAME, s, s.osand, s.ovel);
    }

    let moved = false;
    for (let i = 0; i < G7.grains; i++) {
      const u = s.sand[2 * i]!;
      const v = s.sand[2 * i + 1]!;
      expect(Number.isFinite(u) && Number.isFinite(v)).toBe(true);
      expect(Math.hypot(u, v)).toBeLessThanOrEqual(1 + 1e-6);
      if (u !== before[2 * i] || v !== before[2 * i + 1]) moved = true;
    }
    expect(moved).toBe(true);
    for (let i = 0; i < G4.grains * 2; i++) {
      expect(Math.abs(s.osand[i]!)).toBeLessThanOrEqual(1);
    }
  });

  it("sand at rest does not start moving while the mode is off", () => {
    const s = createSand();
    s.vel.fill(0);
    s.ovel.fill(0);
    const plate = Float32Array.from(s.sand);
    const outer = Float32Array.from(s.osand);

    // An inactive room, however loud it is told to be, nudges nothing: only what was already moving coasts.
    stepSand(G7, plateField("off"), false, 1.3, FRAME, s, s.sand, s.vel);
    stepSand(G4, outerField("off"), false, 1.3, FRAME, s, s.osand, s.ovel);
    expect(s.sand).toEqual(plate);
    expect(s.osand).toEqual(outer);

    // The same step, active, does start it. Without this the check above could pass on a step that
    // never moves anything at all.
    stepSand(G7, plateField("speaking"), true, 1.3, FRAME, s, s.sand, s.vel);
    expect(s.sand).not.toEqual(plate);
  });

  it("the seed makes the same sand every time", () => {
    const a = createSand(11);
    const b = createSand(11);
    expect(Array.from(a.sand.slice(0, 64))).toEqual(Array.from(b.sand.slice(0, 64)));
    expect(createSand(12).sand.slice(0, 64)).not.toEqual(a.sand.slice(0, 64));
  });
});

describe("the room under reduced motion", () => {
  /** A 2D context that records every call. A property read answers a recording function, so any method
   *  the room draws with exists without this list having to name it. */
  function recordingContext() {
    const calls = new Map<string, ReturnType<typeof vi.fn>>();
    const fn = (name: string) => {
      let recorded = calls.get(name);
      if (recorded === undefined) {
        recorded = vi.fn(() => (name.startsWith("create") ? { addColorStop: vi.fn() } : undefined));
        calls.set(name, recorded);
      }
      return recorded;
    };
    const ctx = new Proxy(
      {},
      {
        get: (_target, name) => (typeof name === "string" ? fn(name) : undefined),
        set: () => true,
      },
    );
    return { ctx: ctx as unknown as CanvasRenderingContext2D, fn };
  }

  it("under reduced motion the room paints one still frame", () => {
    const { ctx, fn } = recordingContext();
    const frames = vi.fn(() => 0);
    vi.stubGlobal("requestAnimationFrame", frames);
    vi.stubGlobal(
      "matchMedia",
      vi.fn(() => ({
        matches: true,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    );
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx as never);
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 800,
      height: 600,
    } as DOMRect);

    const { container } = render(<VoiceRoom phase="listening" level={0.3} threshold={0.5} />);

    const canvas = container.querySelector("canvas");
    expect(canvas).not.toBeNull();
    expect(canvas!.getAttribute("aria-hidden")).toBe("true");
    // No loop was started: the sand is settled and drawn once instead.
    expect(frames).not.toHaveBeenCalled();
    expect(fn("fillRect")).toHaveBeenCalled();
  });

  it("under reduced motion the orb paints one still frame", () => {
    const { ctx, fn } = recordingContext();
    const frames = vi.fn(() => 0);
    vi.stubGlobal("requestAnimationFrame", frames);
    vi.stubGlobal(
      "matchMedia",
      vi.fn(() => ({
        matches: true,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    );
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(ctx as never);
    vi.spyOn(HTMLCanvasElement.prototype, "getBoundingClientRect").mockReturnValue({
      width: 800,
      height: 600,
    } as DOMRect);

    const { container } = render(<VoiceOrb phase="speaking" tap={null} />);

    const canvas = container.querySelector("canvas");
    expect(canvas).not.toBeNull();
    expect(canvas!.getAttribute("aria-hidden")).toBe("true");
    // No loop was started: one still frame is drawn instead.
    expect(frames).not.toHaveBeenCalled();
    expect(fn("stroke")).toHaveBeenCalled();
    // What only the new orb draws: the inner dark ball is an arc that is filled. Today's orb strokes
    // lines alone, so without these two the case would pass before the orb was rebuilt.
    expect(fn("arc")).toHaveBeenCalled();
    expect(fn("fill")).toHaveBeenCalled();
  });
});
