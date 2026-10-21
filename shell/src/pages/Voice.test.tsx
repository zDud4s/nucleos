import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

/* The conversation runner, mocked: it owns a microphone, an ONNX model and an `<audio>` element, none
   of which jsdom has. What this page decides about it — when to open a chat, what to draw from the
   view it returns — is exactly what is left to test here. `data/conversation.test.ts` holds the runner
   itself, against a faked audio graph. */
const conversation = vi.hoisted(() => ({
  useVoiceConversation: vi.fn(),
  toggle: vi.fn(),
  pressTalk: vi.fn(),
  releaseTalk: vi.fn(),
}));
vi.mock("../data/conversation", () => conversation);

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Voice } from "./Voice";
import { CONVERSATION_TOGGLE_EVENT, ConversationChord } from "../app/ConversationChord";
import { DictationProvider } from "../app/Dictation";
import { phaseAfter, type Capture, type VoiceConfigView } from "../data/voice";
import { renderWithRouter } from "../test/harness";

/* Inside a router, because the page reads its address: the conversation chord arrives as a `talk`
   stamp in it (`app/ConversationChord.tsx`). And under the shell's dictation, which is where the
   capture it draws lives (`app/Dictation.tsx`). */
function renderVoice(path = "/voice") {
  return renderWithRouter(
    <DictationProvider>
      <Voice />
    </DictationProvider>,
    { initialPath: path },
  );
}

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  mockInvoke.mockReset();
  mockListen.mockReset();
  conversation.toggle.mockReset();
  conversation.useVoiceConversation.mockReset();
  conversation.useVoiceConversation.mockImplementation(() => aConversation());
  window.localStorage.clear();
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
    capture_hotkey: "Ctrl+Alt+N",
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

    await renderVoice();

    expect(await screen.findByText("Voice needs a transcriber")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Start dictation" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Start memo" })).toBeNull();
  });
});

describe("Voice — hotkey registration", () => {
  it("says the hotkeys failed when they did", async () => {
    daemon.apiFetch.mockImplementation(daemonBaseline(voiceConfig()));
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "voice_phase") return "idle";
      if (cmd === "voice_register_hotkeys") throw new Error("host unavailable");
      return undefined;
    });

    await renderVoice();

    expect(await screen.findByText("armed, but the hotkeys did not register: use the buttons below")).toBeDefined();
    const capture = screen.getByRole("group", { name: "Capture" });
    expect(within(capture).getByText("hotkeys failed")).toBeDefined();
    expect(within(capture).queryByText("idle")).toBeNull();
  });

  it("says there are no global hotkeys on Wayland instead of registering them", async () => {
    // The sentence belongs to the host, not to this page: `dictation.rs` answers
    // `voice_hotkeys_unavailable` with it only when the session is Wayland.
    const sentence =
      "this desktop runs Wayland, which gives no application global hotkeys; use the buttons below";
    daemon.apiFetch.mockImplementation(daemonBaseline(voiceConfig()));
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "voice_phase") return "idle";
      if (cmd === "voice_hotkeys_unavailable") return sentence;
      if (cmd === "voice_register_hotkeys") return [];
      return undefined;
    });

    await renderVoice();

    // `textContent` and not a matcher: this suite has no jest-dom.
    await waitFor(() => {
      expect(document.body.textContent).toContain(sentence);
    });
    // The half that matters. Saying the words while still registering chords no
    // compositor will ever deliver is worse than today's silence - the page would
    // claim to have hotkeys and explain that it has none, in the same breath.
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === "voice_register_hotkeys"),
    ).toEqual([]);
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

    await renderVoice();

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

    await renderVoice();

    // The lists live in the archive drawer now, closed until somebody asks for it.
    fireEvent.click(await screen.findByRole("button", { name: /^Archive/ }));

    expect(await screen.findByText("call the plumber")).toBeDefined();
  });
});

/* --------------------------------------------------- the spoken conversation -- */

/** A view in whatever state a test needs, defaulting to a mode nobody has switched on. */
function aConversation(overrides: Record<string, unknown> = {}) {
  return {
    phase: "off",
    heard: null,
    trouble: null,
    hasVoice: true,
    listeningWith: null,
    whyByLoudness: null,
    level: 0,
    threshold: 0.6,
    assembling: null,
    silence: null,
    ignoredEcho: 0,
    answer: null,
    answerTap: null,
    toggle: conversation.toggle,
    pressTalk: conversation.pressTalk,
    releaseTalk: conversation.releaseTalk,
    ...overrides,
  };
}

/** The baseline, plus the chat list the one spoken conversation is looked up in. */
function withChats(
  config: VoiceConfigView,
  listed: Array<{ chat_id: string; title: string | null }>,
  opens: string | Error = "c-voice",
) {
  const base = daemonBaseline(config);
  return async (path: string, init?: RequestInit) => {
    if (path === "/assistant/chats" && (init?.method ?? "GET") === "GET") return listed;
    if (path === "/assistant/chats" && init?.method === "POST") {
      if (opens instanceof Error) throw opens;
      return { chat_id: opens };
    }
    if (path.startsWith("/assistant/chats/") && init?.method === "PATCH") return undefined;
    return base(path, init);
  };
}

/** Every call the daemon was asked for, as `METHOD /path`. */
function called(): string[] {
  return daemon.apiFetch.mock.calls.map(
    (call) => `${(call[1] as RequestInit | undefined)?.method ?? "GET"} ${String(call[0])}`,
  );
}

describe("Voice — the spoken conversation", () => {
  it("says what is missing instead of offering a button it cannot honour", async () => {
    daemon.apiFetch.mockImplementation(daemonBaseline(voiceConfig({ armed: false, hotkey: "", memo_hotkey: "" })));

    await renderVoice();

    expect(await screen.findByText("no transcriber is set up: spoken turns cannot be understood yet")).toBeDefined();
    expect(screen.queryByRole("button", { name: "Start talking" })).toBeNull();
  });

  /* The order is the point, not an implementation detail: the daemon refuses a turn that names no
     chat, so opening one has to finish BEFORE the microphone does. A toggle that went first would
     leave a live microphone whose first sentence could not be sent anywhere. */
  it("opens the one spoken conversation before it opens the microphone", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));

    await renderVoice();
    fireEvent.click(await screen.findByRole("button", { name: "Start talking" }));

    await waitFor(() => expect(conversation.toggle).toHaveBeenCalledTimes(1));
    expect(called()).toContain("POST /assistant/chats");
    expect(window.localStorage.getItem("nucleos.voice-chat")).toBe("c-voice");
    /* That the opening comes FIRST is proved by the test below rather than by an index here: nothing
       in this list is the toggle, so their order cannot be read from it. A run where opening fails and
       the microphone stays shut can only happen if the toggle waits on it. */
  });

  it("speaks into the conversation it already has, without opening another", async () => {
    daemon.apiFetch.mockImplementation(
      withChats(voiceConfig(), [{ chat_id: "c-voice", title: "Voice" }]),
    );

    await renderVoice();
    fireEvent.click(await screen.findByRole("button", { name: "Start talking" }));

    await waitFor(() => expect(conversation.toggle).toHaveBeenCalledTimes(1));
    expect(called()).not.toContain("POST /assistant/chats");
  });

  /* A microphone opened with nowhere to send what it hears is the failure this pillar's design keeps
     naming: armed-looking and useless. Better to stay off and say why. */
  it("does not open the microphone when no conversation could be opened", async () => {
    daemon.apiFetch.mockImplementation(
      withChats(voiceConfig(), [], new Error("the daemon is not answering")),
    );

    await renderVoice();
    fireEvent.click(await screen.findByRole("button", { name: "Start talking" }));

    expect(await screen.findByText("the daemon is not answering")).toBeDefined();
    expect(conversation.toggle).not.toHaveBeenCalled();
  });

  /* The meter and the bar are one reading: "it hears me this much, and this much would open a turn".
     Drawn separately they would answer neither — which is why the threshold travels in the accessible
     text too, rather than living only in a coloured pixel. */
  it("draws the level against the bar actually in force", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));
    conversation.useVoiceConversation.mockImplementation(() =>
      aConversation({ phase: "speaking", level: 0.3, threshold: 0.85 }),
    );

    await renderVoice();

    const meter = await screen.findByRole("meter", { name: "microphone level" });
    expect(meter.getAttribute("aria-valuenow")).toBe("0.3");
    expect(meter.getAttribute("aria-valuetext")).toBe("level 30%, a turn opens at 85%");
  });

  /* The orb is the same reading as the meter, drawn for the eye. Exposed to a screen reader it would
     be an unnamed graphic announced beside the meter that already says everything it shows. */
  it("draws the orb as decoration beside the meter that carries the reading", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));
    conversation.useVoiceConversation.mockImplementation(() =>
      aConversation({ phase: "hearing", level: 0.9, threshold: 0.85 }),
    );

    const { container } = await renderVoice();

    const meter = await screen.findByRole("meter", { name: "microphone level" });
    // 0.9 is past the 0.85 bar: the fill changes tone to say a turn is open.
    expect(meter.hasAttribute("data-open")).toBe(true);
    const orb = container.querySelector("canvas.voice-orb");
    expect(orb?.getAttribute("aria-hidden")).toBe("true");
    expect(orb?.hasAttribute("data-hearing")).toBe(true);
    // The phase is a plain attribute too, so reduced motion can tell phases apart in CSS.
    expect(orb?.getAttribute("data-phase")).toBe("hearing");
    // A live conversation keeps the dock showing even when the archive is open on a narrow window.
    expect(container.querySelector(".voice-room")?.hasAttribute("data-live")).toBe(true);
  });

  /* A segment dropped in silence looks exactly like a microphone that failed. Saying it was the
     assistant's own voice is the difference between a working guard and an apparent fault. */
  it("says when it heard its own answer and refused to treat it as a turn", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));
    conversation.useVoiceConversation.mockImplementation(() =>
      aConversation({ phase: "listening", ignoredEcho: 2 }),
    );

    await renderVoice();

    expect(await screen.findByText("ignored its own voice ×2")).toBeDefined();
  });

  /* Not the alarmed question it used to be. `energyOf` bottoms out at -50 dBFS and an ordinary quiet
     room sits about there, so this reason cannot tell a silent room from a dead device — and the
     sentence must not pretend it can. */
  it("reports silence as a fact rather than diagnosing the microphone", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));
    conversation.useVoiceConversation.mockImplementation(() =>
      aConversation({ phase: "listening", silence: "flat" }),
    );

    await renderVoice();

    expect(await screen.findByText("silence: nothing is reaching the microphone")).toBeDefined();
  });
});

describe("Voice — the conversation chord", () => {
  /* The chord arrives as a stamp in the address, from whatever page was open. It has to take the
     button's own path — conversation first, then the microphone — or the first press of a fresh
     install switches the mode on with no conversation, and the daemon refuses the first turn. */
  it("starts talking through the button's own path when the chord brought the person here", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig(), []));

    const { router } = await renderVoice("/voice?talk=1700000000000");

    await waitFor(() => expect(conversation.toggle).toHaveBeenCalledTimes(1));
    expect(called()).toContain("POST /assistant/chats");
    expect(window.localStorage.getItem("nucleos.voice-chat")).toBe("c-voice");
    // Consumed, so a reload or a Back does not start a conversation nobody asked for this time.
    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it("does nothing but clear the stamp on a page that cannot transcribe", async () => {
    daemon.apiFetch.mockImplementation(withChats(voiceConfig({ armed: false }), []));

    const { router } = await renderVoice("/voice?talk=1700000000000");

    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect(conversation.toggle).not.toHaveBeenCalled();
    expect(called()).not.toContain("POST /assistant/chats");
  });

  /* A second press is a second stamp, and it must reach the SAME page instance: the chord pressed
     while the conversation is on is how somebody stops it without looking for the button. */
  it("answers a second press on the page that is already open", async () => {
    daemon.apiFetch.mockImplementation(
      withChats(voiceConfig(), [{ chat_id: "c-voice", title: "Voice" }]),
    );

    let chord: (() => void) | null = null;
    mockListen.mockImplementation(async (event, handler) => {
      if (event === CONVERSATION_TOGGLE_EVENT) chord = () => handler({} as never);
      return () => {};
    });

    // The page and the shell's listener together, as the app mounts them.
    const { router } = await renderWithRouter(
      <DictationProvider>
        <Voice />
        <ConversationChord />
      </DictationProvider>,
      { initialPath: "/voice?talk=1" },
    );
    await waitFor(() => expect(conversation.toggle).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(router.state.location.search).toEqual({}));
    await waitFor(() => expect(chord).not.toBeNull());

    act(() => chord?.());

    await waitFor(() => expect(conversation.toggle).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });
});

/* ------------------------------------------------------------------ the room -- */

/** The page with the conversation in whatever state a test needs, and a daemon that has no chats. */
async function renderRoom(
  view: Record<string, unknown>,
  config: VoiceConfigView = voiceConfig(),
  memos: Capture[] = [],
  dictations: Capture[] = [],
) {
  const base = withChats(config, []);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (path === "/voice/memos") return memos;
    if (path === "/voice/dictations") return dictations;
    return base(path, init);
  });
  conversation.useVoiceConversation.mockImplementation(() => aConversation(view));
  return renderVoice();
}

/** The captions block, once the page has drawn it. */
async function findCaptions(container: HTMLElement): Promise<HTMLElement> {
  await screen.findByRole("button", { name: /^(Start|Stop) talking$/ });
  const captions = container.querySelector<HTMLElement>(".voice-captions");
  expect(captions).not.toBeNull();
  return captions as HTMLElement;
}

describe("Voice — the room", () => {
  /* What it says is read from the real view, in the order the person lived it: what they said first,
     then what the núcleo answered. The label names WHOSE words these are, so the lines below are
     told apart by it and not by position. */
  it("captions show the answer's words while it is spoken", async () => {
    let { container, unmount } = await renderRoom({
      phase: "speaking",
      heard: "when is the meeting",
      answer: "The meeting is at three o'clock.",
    });
    let captions = await findCaptions(container);

    // A live region, so the words are read out as they change rather than only found by a cursor.
    expect(captions.getAttribute("aria-live")).toBe("polite");
    expect(captions.querySelector(".voice-caption-label")?.textContent).toBe("nucleos");
    expect(captions.querySelector(".voice-caption-words")?.textContent).toContain(
      "The meeting is at three o'clock.",
    );
    unmount();

    // Heard and still being answered: the person's own words, with what is happening to them.
    ({ container, unmount } = await renderRoom({ phase: "thinking", heard: "what time is it" }));
    captions = await findCaptions(container);
    expect(captions.querySelector(".voice-caption-label")?.textContent).toBe("heard: sending");
    expect(captions.querySelector(".voice-caption-words")?.textContent).toContain("“what time is it”");
    unmount();

    // Heard while listening: sent already, so no "sending".
    ({ container, unmount } = await renderRoom({ phase: "listening", heard: "what time is it" }));
    captions = await findCaptions(container);
    expect(captions.querySelector(".voice-caption-label")?.textContent).toBe("heard");
    expect(captions.querySelector(".voice-caption-words")?.textContent).toContain("“what time is it”");
    unmount();

    // Switched off, the last thing heard stays on the wall, named as old.
    ({ container, unmount } = await renderRoom({ phase: "off", heard: "what time is it" }));
    captions = await findCaptions(container);
    expect(captions.querySelector(".voice-caption-label")?.textContent).toBe("last heard");
    expect(captions.querySelector(".voice-caption-words")?.textContent).toContain("“what time is it”");
    unmount();

    // Armed, off, and nothing heard yet: the one line that says how to begin, hotkey included.
    ({ container, unmount } = await renderRoom({ phase: "off", heard: null }));
    captions = await findCaptions(container);
    expect(captions.querySelector(".voice-caption-quiet")?.textContent).toBe(
      "press Start talking, or Ctrl+Alt+C from anywhere, and speak",
    );
    unmount();

    // A machine with no conversation hotkey must not be told to press one.
    ({ container } = await renderRoom(
      { phase: "off", heard: null },
      voiceConfig({ conversation_hotkey: "" }),
    ));
    captions = await findCaptions(container);
    const quiet = captions.querySelector(".voice-caption-quiet");
    expect(quiet).not.toBeNull();
    expect(quiet?.textContent).not.toContain("from anywhere");
  });

  it("captions show what is being assembled while hearing", async () => {
    const { container } = await renderRoom({
      phase: "hearing",
      level: 0.9,
      assembling: "buy some mi",
    });
    const captions = await findCaptions(container);

    expect(captions.querySelector(".voice-caption-label")?.textContent).toBe("hearing");
    const words = captions.querySelector(".voice-caption-words");
    expect(words?.textContent).toContain("buy some mi");
    // The cursor of a sentence still being written is decoration; a screen reader must not read it.
    const caret = captions.querySelector(".voice-caption-caret");
    expect(caret).not.toBeNull();
    expect(caret?.getAttribute("aria-hidden")).toBe("true");
    expect(words?.contains(caret as Element)).toBe(true);

    // They live in the captions now; the dock's status must not say them a second time.
    expect(screen.queryByText(/^hearing: /)).toBeNull();
    expect(screen.queryByText(/^heard: /)).toBeNull();
  });

  it("the dock names each capture's hotkey without renaming its button", async () => {
    const { container, unmount } = await renderRoom({ phase: "off" });

    // The key is a hint for the eye; the button's accessible name is what it always was.
    const dictate = await screen.findByRole("button", { name: "Start dictation" });
    const memo = screen.getByRole("button", { name: "Start memo" });
    const talk = screen.getByRole("button", { name: "Start talking" });
    for (const [button, key] of [
      [dictate, "Ctrl+Alt+D"],
      [memo, "Ctrl+Alt+M"],
      [talk, "Ctrl+Alt+C"],
    ] as const) {
      const kbd = button.querySelector("kbd.voice-kbd");
      expect(kbd?.textContent).toBe(key);
      expect(kbd?.getAttribute("aria-hidden")).toBe("true");
    }

    // The two captures sit together under one name; the conversation's button does not.
    const group = screen.getByRole("group", { name: "Capture" });
    expect(within(group).getByRole("button", { name: "Start dictation" })).toBe(dictate);
    expect(within(group).getByRole("button", { name: "Start memo" })).toBe(memo);
    expect(within(group).queryByRole("button", { name: "Start talking" })).toBeNull();
    expect(container.querySelector(".voice-dock-talk")?.contains(talk)).toBe(true);

    // The microphone being shut is said first, in the diagnostics slot.
    expect(container.querySelector(".voice-dock-diag")?.textContent).toContain("microphone closed");
    unmount();

    // No hotkey configured, no key drawn: an empty chip beside a button is worse than none.
    await renderRoom(
      { phase: "off" },
      voiceConfig({ hotkey: "", memo_hotkey: "", conversation_hotkey: "" }),
    );
    for (const name of ["Start dictation", "Start memo", "Start talking"]) {
      const button = await screen.findByRole("button", { name });
      expect(button.querySelector("kbd")).toBeNull();
    }
  });

  it("the archive opens on memos, switches tabs, and closes on escape back to its button", async () => {
    const { container } = await renderRoom(
      { phase: "off" },
      voiceConfig(),
      [capture({ id: 7, raw_text: "call the plumber" }), capture({ id: 8, raw_text: "water the plants" })],
      [capture({ id: 9, kind: "dictation", raw_text: "turn the heating down" })],
    );

    const opener = await screen.findByRole("button", { name: /^Archive/ });
    await waitFor(() => expect(opener.getAttribute("aria-label")).toContain("2 memos"));
    expect(opener.getAttribute("aria-label")).toContain("1 dictation");
    expect(opener.getAttribute("aria-label")).not.toContain("1 dictations");
    expect(opener.getAttribute("aria-expanded")).toBe("false");
    expect(opener.getAttribute("aria-controls")).toBe("voice-archive");
    // Closed, the drawer is not on the page at all, so nothing in it can be tabbed to.
    expect(screen.queryByRole("dialog", { name: "Voice archive" })).toBeNull();
    expect(container.querySelector("#voice-archive")).toBeNull();

    fireEvent.click(opener);

    const drawer = await screen.findByRole("dialog", { name: "Voice archive" });
    expect(drawer.closest("#voice-archive")).not.toBeNull();
    expect(opener.getAttribute("aria-expanded")).toBe("true");
    // Focus moves in, onto the drawer itself, so the first Tab lands on the first control.
    await waitFor(() => expect(document.activeElement).toBe(drawer));

    // It opens on the memos, and the other lists are one tab away.
    expect(within(drawer).getByRole("tablist", { name: "Archive sections" })).toBeDefined();
    expect(within(drawer).getByRole("tab", { name: /Memos/ }).getAttribute("aria-selected")).toBe("true");
    expect(await within(drawer).findByText("call the plumber")).toBeDefined();
    expect(within(drawer).queryByText("turn the heating down")).toBeNull();

    // Radix selects on mousedown, never on click.
    fireEvent.mouseDown(within(drawer).getByRole("tab", { name: /Dictations/ }));
    expect(await within(drawer).findByText("turn the heating down")).toBeDefined();
    expect(within(drawer).queryByText("call the plumber")).toBeNull();
    expect(within(drawer).getByRole("tab", { name: /Configuration/ })).toBeDefined();

    // Escape inside the drawer closes it and hands focus back to the button that opened it; one aimed
    // at the window does not (it belongs to whatever else is on the page).
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Voice archive" })).not.toBeNull();
    fireEvent.keyDown(drawer, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Voice archive" })).toBeNull());
    expect(document.activeElement).toBe(opener);
    expect(opener.getAttribute("aria-expanded")).toBe("false");

    // And so does its own Close button, with the same hand-back.
    fireEvent.click(opener);
    fireEvent.click(
      within(await screen.findByRole("dialog", { name: "Voice archive" })).getByRole("button", {
        name: "Close",
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Voice archive" })).toBeNull());
    expect(document.activeElement).toBe(opener);
  });

  it("the room is marked while the archive is open, so the dock can clear it", async () => {
    const { container } = await renderRoom({ phase: "off" });
    const opener = await screen.findByRole("button", { name: /^Archive/ });
    const room = container.querySelector(".voice-room");
    expect(room).not.toBeNull();
    expect(room?.getAttribute("data-archive")).toBeNull();

    fireEvent.click(opener);
    const drawer = await screen.findByRole("dialog", { name: "Voice archive" });
    expect(room?.getAttribute("data-archive")).toBe("open");
    // Idle, so the narrow layout may hide the dock; no `data-live` to keep it showing.
    expect(room?.getAttribute("data-live")).toBeNull();

    fireEvent.keyDown(drawer, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Voice archive" })).toBeNull());
    expect(room?.getAttribute("data-archive")).toBeNull();
  });

  it("the room is decoration", async () => {
    const { container } = await renderRoom({ phase: "hearing", level: 0.9, threshold: 0.85 });
    await screen.findByRole("meter", { name: "microphone level" });

    // The room carries the phase for the stylesheet, and its canvas is hidden from assistive tech.
    const room = container.querySelector(".voice-room");
    expect(room).not.toBeNull();
    expect(room?.getAttribute("data-phase")).toBe("hearing");
    const canvas = container.querySelector("canvas.voice-room-canvas");
    expect(canvas).not.toBeNull();
    expect(canvas?.getAttribute("aria-hidden")).toBe("true");
    expect(room?.contains(canvas as Element)).toBe(true);
    // The orb stays where it was, inside the room.
    expect(room?.contains(container.querySelector("canvas.voice-orb"))).toBe(true);
  });
});

describe("Voice — push-to-talk", () => {
  beforeEach(() => {
    conversation.pressTalk.mockReset();
    conversation.releaseTalk.mockReset();
  });

  it("asks the conversation for push-to-talk", async () => {
    await renderRoom({ phase: "listening" });

    // The chat id is null here — this daemon has no chats — so only the options are asserted.
    const options = conversation.useVoiceConversation.mock.calls.map((call) => call[1]);
    expect(options).toContainEqual(expect.objectContaining({ pushToTalk: true }));
  });

  it("holding Space talks and letting go sends, once per press", async () => {
    await renderRoom({ phase: "listening" });
    await screen.findByRole("button", { name: /^(Start|Stop) talking$/ });

    fireEvent.keyDown(window, { key: " ", code: "Space" });
    fireEvent.keyDown(window, { key: " ", code: "Space", repeat: true });
    fireEvent.keyUp(window, { key: " ", code: "Space" });

    expect(conversation.pressTalk).toHaveBeenCalledTimes(1);
    expect(conversation.releaseTalk).toHaveBeenCalledTimes(1);
  });

  it("Space typed into a text field is a space, not a turn", async () => {
    await renderRoom({ phase: "listening" });
    await screen.findByRole("button", { name: /^(Start|Stop) talking$/ });
    const field = document.createElement("input");
    document.body.appendChild(field);

    fireEvent.keyDown(field, { key: " ", code: "Space" });
    fireEvent.keyUp(field, { key: " ", code: "Space" });

    expect(conversation.pressTalk).not.toHaveBeenCalled();
    field.remove();
  });

  it("Space does nothing while the mode is off", async () => {
    await renderRoom({ phase: "off" });
    await screen.findByRole("button", { name: /^(Start|Stop) talking$/ });

    fireEvent.keyDown(window, { key: " ", code: "Space" });

    expect(conversation.pressTalk).not.toHaveBeenCalled();
  });
});
