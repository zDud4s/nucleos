import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  apiFetch: daemon.apiFetch,
}));

import { DictationProvider, useDictation } from "./Dictation";

type Handler = (event: { payload?: unknown }) => void;
let handlers: Record<string, Handler>;

/** jsdom has neither `getUserMedia` nor `AudioContext`; this is enough to capture zero frames and post them. */
function stubAudioPipeline(
  getUserMedia: () => Promise<MediaStream> = () =>
    Promise.resolve({ getTracks: () => [{ stop: vi.fn() }] } as unknown as MediaStream),
  closeContext: () => Promise<void> = () => Promise.resolve(),
) {
  Object.defineProperty(window.navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia: vi.fn(getUserMedia) },
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
      return closeContext();
    }
  }
  (window as unknown as { AudioContext: typeof AudioContext }).AudioContext =
    FakeAudioContext as unknown as typeof AudioContext;
}

/** Some other page: it knows nothing about voice, and draws only the phase so the test can read it. */
function SomeOtherPage() {
  return <p>{useDictation().phase}</p>;
}

beforeEach(() => {
  handlers = {};
  stubAudioPipeline();
  tauri.invoke.mockReset();
  tauri.invoke.mockImplementation(async (command: string) => {
    if (command === "voice_phase") return "idle";
    if (command === "voice_paste") return { pasted: true, held: null };
    return undefined;
  });
  tauri.listen.mockReset();
  tauri.listen.mockImplementation(async (event: string, handler: Handler) => {
    handlers[event] = handler;
    return () => {};
  });
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({ id: 7, text: "hello from anywhere" });
});

describe("DictationProvider", () => {
  /* The defect it exists for: pressed away from the Voice page, the chord moved the host to recording
     with no microphone, and the second press left it transcribing for good — nothing sent the paste
     or the abandon that bring it back. */
  it("records, transcribes and pastes a chord pressed on any page", async () => {
    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );
    await waitFor(() => expect(handlers["voice://stop"]).toBeDefined());

    act(() => handlers["voice://start"]({ payload: "dictation" }));
    expect(await screen.findByText("recording")).toBeDefined();

    act(() => handlers["voice://stop"]({}));

    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("voice_paste", { text: "hello from anywhere" }),
    );
    expect(String(daemon.apiFetch.mock.calls[0][0])).toContain("kind=dictation");
    expect(await screen.findByText("idle")).toBeDefined();
  });

  it("files a memo without pasting it", async () => {
    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );
    await waitFor(() => expect(handlers["voice://stop"]).toBeDefined());

    act(() => handlers["voice://start"]({ payload: "memo" }));
    expect(await screen.findByText("recording")).toBeDefined();
    act(() => handlers["voice://stop"]({}));

    expect(await screen.findByText("idle")).toBeDefined();
    expect(String(daemon.apiFetch.mock.calls[0][0])).toContain("kind=memo");
    expect(tauri.invoke).not.toHaveBeenCalledWith("voice_paste", expect.anything());
  });

  it("stays quiet where there is no host to listen to", async () => {
    tauri.listen.mockRejectedValue(new Error("no Tauri runtime"));
    tauri.invoke.mockRejectedValue(new Error("no Tauri runtime"));

    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );

    await waitFor(() => expect(tauri.listen).toHaveBeenCalledTimes(2));
    expect(screen.getByText("idle")).toBeDefined();
  });

  /* The host moved to `recording` the moment the chord fired; a microphone that then cannot open must
     hand it back, or every later hotkey answers "busy" until the app restarts. */
  it("abandons the host when the microphone cannot be opened", async () => {
    stubAudioPipeline(() => Promise.reject(new Error("permission denied")));
    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );
    await waitFor(() => expect(handlers["voice://stop"]).toBeDefined());

    act(() => handlers["voice://start"]({ payload: "dictation" }));

    await waitFor(() => expect(tauri.invoke).toHaveBeenCalledWith("voice_abandon"));
    expect(await screen.findByText("idle")).toBeDefined();
  });

  it("honours a stop that arrives while the permission prompt is open", async () => {
    let allow: () => void = () => {};
    stubAudioPipeline(
      () =>
        new Promise((resolve) => {
          allow = () => resolve({ getTracks: () => [{ stop: vi.fn() }] } as unknown as MediaStream);
        }),
    );
    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );
    await waitFor(() => expect(handlers["voice://stop"]).toBeDefined());

    act(() => handlers["voice://start"]({ payload: "dictation" }));
    act(() => handlers["voice://stop"]({}));
    await act(async () => allow());

    await waitFor(() =>
      expect(tauri.invoke).toHaveBeenCalledWith("voice_paste", { text: "hello from anywhere" }),
    );
    expect(await screen.findByText("idle")).toBeDefined();
  });

  it("abandons the host when closing the microphone fails", async () => {
    stubAudioPipeline(undefined, () => Promise.reject(new Error("context gone")));
    render(
      <DictationProvider>
        <SomeOtherPage />
      </DictationProvider>,
    );
    await waitFor(() => expect(handlers["voice://stop"]).toBeDefined());

    act(() => handlers["voice://start"]({ payload: "dictation" }));
    expect(await screen.findByText("recording")).toBeDefined();
    act(() => handlers["voice://stop"]({}));

    await waitFor(() => expect(tauri.invoke).toHaveBeenCalledWith("voice_abandon"));
    expect(await screen.findByText("idle")).toBeDefined();
  });
});
