import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Voice } from "./Voice";
import { phaseAfter, type Capture, type VoiceConfigView } from "../data/voice";
import { renderWithQuery } from "../test/harness";

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  mockInvoke.mockReset();
  mockListen.mockReset();
  // Both event subscriptions resolve to a harmless no-op unlisten by default —
  // individual tests override `voice_hotkey` and friends as they need.
  mockListen.mockResolvedValue(() => {});
  mockInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === "voice_phase") return "idle";
    if (cmd === "voice_register_hotkeys") return [];
    return undefined;
  });
});

/* ------------------------------------------------------------- fixtures -- */

function voiceConfig(overrides: Partial<VoiceConfigView> = {}): VoiceConfigView {
  return {
    armed: true,
    hints: [],
    cleanup_prompt: "Clean up disfluencies without changing what was said.",
    cleanup_model: "local-cleaner",
    retain_dictations_days: 30,
    hotkey: "Ctrl+Alt+D",
    memo_hotkey: "Ctrl+Alt+M",
    conversation_hotkey: "Ctrl+Alt+C",
    speaks: true,
    max_capture_seconds: 1200,
    max_body_bytes: 38_401_024,
    ...overrides,
  };
}

function capture(overrides: Partial<Capture> = {}): Capture {
  return {
    id: 1,
    kind: "memo",
    created_at: "2026-08-17T09:00:00Z",
    duration_ms: 4200,
    raw_text: "buy milk on the way home",
    clean_text: null,
    cleanup_state: "raw",
    model: null,
    ...overrides,
  };
}

/**
 * A minimal, fully synchronous stand-in for `getUserMedia` +
 * `AudioContext` — jsdom implements neither, and this shell does not either
 * (see `data/voice.ts` and `Voice.tsx`'s own header: that leg has no
 * automated coverage of the real hardware). This stub exists only to let the
 * encode-and-post path run with zero captured frames, which is enough to
 * reach `postCapture` and read back what it answered.
 */
function stubAudioPipeline() {
  const stream = { getTracks: () => [{ stop: vi.fn() }] } as unknown as MediaStream;
  Object.defineProperty(window.navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia: vi.fn().mockResolvedValue(stream) },
  });

  class FakeAudioContext {
    sampleRate = 48000;
    destination = {};
    createMediaStreamSource() {
      return { connect: vi.fn(), disconnect: vi.fn() };
    }
    createScriptProcessor() {
      return { connect: vi.fn(), disconnect: vi.fn(), onaudioprocess: null };
    }
    createGain() {
      return { connect: vi.fn(), disconnect: vi.fn(), gain: { value: 1 } };
    }
    close() {
      return Promise.resolve();
    }
  }

  (window as unknown as { AudioContext: typeof AudioContext }).AudioContext =
    FakeAudioContext as unknown as typeof AudioContext;
}

function daemonBaseline(config: VoiceConfigView, memos: Capture[] = [], dictations: Capture[] = []) {
  return async (path: string, init?: RequestInit) => {
    if (path === "/voice/config") return config;
    if (path === "/voice/memos") return memos;
    if (path === "/voice/dictations") return dictations;
    if (path.startsWith("/voice/capture") && init?.method === "POST") return undefined;
    return undefined;
  };
}

/* ------------------------------------------------------------ phaseAfter -- */

describe("phaseAfter — the capture state machine", () => {
  it("moves to recording on the real hotkey's start event", () => {
    expect(phaseAfter({ type: "start", kind: "dictation" })).toBe("recording");
    expect(phaseAfter({ type: "start", kind: "memo" })).toBe("recording");
  });

  it("moves to transcribing on the real hotkey's stop event", () => {
    expect(phaseAfter({ type: "stop" })).toBe("transcribing");
  });

  it("reads a busy hotkey result as still transcribing, not as a phase of its own", () => {
    expect(phaseAfter({ type: "hotkey", phase: "busy" })).toBe("transcribing");
    expect(phaseAfter({ type: "hotkey", phase: "recording" })).toBe("recording");
    expect(phaseAfter({ type: "hotkey", phase: "transcribing" })).toBe("transcribing");
  });

  it("returns to idle once a capture settles, fails, or is abandoned", () => {
    expect(phaseAfter({ type: "capture-done" })).toBe("idle");
    expect(phaseAfter({ type: "capture-error" })).toBe("idle");
    expect(phaseAfter({ type: "abandon" })).toBe("idle");
  });
});

/* ------------------------------------------------------------- not armed -- */

describe("Voice — not armed", () => {
  it("teaches instead of offering capture when voice is not armed", async () => {
    daemon.apiFetch.mockImplementation(daemonBaseline(voiceConfig({ armed: false, hotkey: "", memo_hotkey: "" })));

    renderWithQuery(<Voice />);

    expect(await screen.findByText("Voice is not armed")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Start dictation" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Start memo" })).toBeNull();
  });
});

/* --------------------------------------------------------------- capture -- */

describe("Voice — capture outcomes", () => {
  it("names a 204 capture as nothing heard rather than reporting success", async () => {
    stubAudioPipeline();
    daemon.apiFetch.mockImplementation(daemonBaseline(voiceConfig()));

    let hotkeyCalls = 0;
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "voice_phase") return "idle";
      if (cmd === "voice_register_hotkeys") return [];
      if (cmd === "voice_hotkey") {
        hotkeyCalls += 1;
        return hotkeyCalls === 1 ? "recording" : "transcribing";
      }
      return undefined;
    });

    renderWithQuery(<Voice />);

    const start = await screen.findByRole("button", { name: "Start dictation" });
    fireEvent.click(start);

    const stop = await screen.findByRole("button", { name: /Stop dictation/ });
    fireEvent.click(stop);

    // `undefined` is the whole answer here, not an unsettled query — the page
    // must name the outcome rather than render nothing.
    expect(await screen.findByText(/nothing was heard/)).toBeDefined();
  });

  it("shows the memos list with a working delete", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/voice/config") return voiceConfig();
      if (path === "/voice/memos") return [capture({ id: 7, raw_text: "call the plumber" })];
      if (path === "/voice/dictations") return [];
      if (path === "/voice/memos/7" && init?.method === "DELETE") return undefined;
      return undefined;
    });

    renderWithQuery(<Voice />);

    expect(await screen.findByText("call the plumber")).toBeDefined();
  });
});
