import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke, isTauri: () => false }));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => undefined) }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  apiFetch: daemon.apiFetch,
}));

import { DictationProvider, keepFocus, useDictation, type Dictation } from "../app/Dictation";
import { chordMatches, pageChord } from "../lib/dictationTarget";
import { desktopTransport, setTransport } from "../data/transport";
import { remoteTransport } from "./transport";

let latest: Dictation;

function Probe() {
  latest = useDictation();
  return <p data-testid="phase">{latest.phase}</p>;
}

function stubMicrophone(getUserMedia?: () => Promise<MediaStream>) {
  Object.defineProperty(window.navigator, "mediaDevices", {
    configurable: true,
    value: getUserMedia === undefined ? undefined : { getUserMedia: vi.fn(getUserMedia) },
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

const okStream = () => Promise.resolve({ getTracks: () => [{ stop: vi.fn() }] } as unknown as MediaStream);

function stubClipboard(writeText: (text: string) => Promise<void>) {
  Object.defineProperty(window.navigator, "clipboard", { configurable: true, value: { writeText: vi.fn(writeText) } });
}

/** One full dictation: press to start, press to stop. */
async function dictate() {
  await act(async () => {
    await latest.press(false);
  });
  expect(screen.getByTestId("phase").textContent).toBe("recording");
  await act(async () => {
    await latest.press(false);
  });
}

beforeEach(() => {
  setTransport(remoteTransport);
  tauri.invoke.mockReset();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({ id: 1, text: "hello laptop" });
  stubMicrophone(okStream);
  stubClipboard(() => Promise.resolve());
});

afterEach(() => {
  setTransport(desktopTransport);
  document.body.innerHTML = "";
});

describe("dictation in the browser", () => {
  it("dictation_in_the_browser_posts_to_voice_capture", async () => {
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await dictate();
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(1));
    const [url, init] = daemon.apiFetch.mock.calls[0];
    expect(String(url)).toContain("/voice/capture?");
    expect(String(url)).toContain("kind=dictation");
    expect(init.method).toBe("POST");
    expect(tauri.invoke).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.getByTestId("phase").textContent).toBe("idle"));
  });

  it("dictated_text_lands_in_the_focused_field_or_clipboard", async () => {
    render(
      <>
        <input aria-label="note" defaultValue="" />
        <DictationProvider>
          <Probe />
        </DictationProvider>
      </>,
    );
    // With a field focused: typed into it, clipboard untouched.
    const field = screen.getByLabelText("note") as HTMLInputElement;
    field.focus();
    await dictate();
    await waitFor(() => expect(field.value).toBe("hello laptop"));
    expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
    expect(latest.delivery?.pasted).toBe(true);

    // With focus on a button: the clipboard.
    (document.activeElement as HTMLElement).blur();
    await dictate();
    await waitFor(() => expect(navigator.clipboard.writeText).toHaveBeenCalledWith("hello laptop"));
    expect(field.value).toBe("hello laptop");
    expect(latest.delivery?.pasted).toBe(false);
  });

  it("updates a React-controlled input, not just the DOM value", async () => {
    function Controlled() {
      const [value, setValue] = useState("a ");
      return (
        <>
          <input aria-label="c" value={value} onChange={(e) => setValue(e.target.value)} />
          <p data-testid="state">{value}</p>
        </>
      );
    }
    render(
      <DictationProvider>
        <Controlled />
        <Probe />
      </DictationProvider>,
    );
    const field = screen.getByLabelText("c") as HTMLInputElement;
    field.focus();
    field.setSelectionRange(2, 2);
    await dictate();
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("a hello laptop"));
    expect(field.value).toBe("a hello laptop");
  });

  it("writes to the field focused when recording STARTED, even if focus moved", async () => {
    render(
      <>
        <input aria-label="first" />
        <input aria-label="second" />
        <DictationProvider>
          <Probe />
        </DictationProvider>
      </>,
    );
    const first = screen.getByLabelText("first") as HTMLInputElement;
    const second = screen.getByLabelText("second") as HTMLInputElement;
    first.focus();
    await act(async () => {
      await latest.press(false);
    });
    second.focus();
    await act(async () => {
      await latest.press(false);
    });
    await waitFor(() => expect(first.value).toBe("hello laptop"));
    expect(second.value).toBe("");
  });

  it("falls back to the clipboard when the starting field is gone", async () => {
    const { unmount } = render(<input aria-label="gone" />);
    const field = screen.getByLabelText("gone") as HTMLInputElement;
    field.focus();
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await act(async () => {
      await latest.press(false);
    });
    unmount();
    await act(async () => {
      await latest.press(false);
    });
    await waitFor(() => expect(navigator.clipboard.writeText).toHaveBeenCalledWith("hello laptop"));
  });

  it("shows the text when the clipboard write is refused", async () => {
    stubClipboard(() => Promise.reject(new Error("denied")));
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await dictate();
    await waitFor(() => expect(latest.delivery?.held).toContain("hello laptop"));
    expect(latest.delivery?.pasted).toBe(false);
  });

  it("shows the text when there is no clipboard at all", async () => {
    Object.defineProperty(window.navigator, "clipboard", { configurable: true, value: undefined });
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await dictate();
    await waitFor(() => expect(latest.delivery?.held).toContain("hello laptop"));
  });

  it("explains a refused microphone and does not stay recording", async () => {
    const denied = new Error("denied");
    denied.name = "NotAllowedError";
    stubMicrophone(() => Promise.reject(denied));
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await act(async () => {
      await latest.press(false);
    });
    expect(latest.outcome).toEqual({ kind: "mic-error", reason: "denied" });
    expect(screen.getByTestId("phase").textContent).toBe("idle");
    expect(latest.activeKind).toBeNull();
    // A second press starts afresh rather than being read as a stop.
    stubMicrophone(okStream);
    await act(async () => {
      await latest.press(false);
    });
    expect(screen.getByTestId("phase").textContent).toBe("recording");
  });

  it("explains a missing getUserMedia (not HTTPS) and does not stay recording", async () => {
    stubMicrophone(undefined);
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await act(async () => {
      await latest.press(false);
    });
    expect(latest.outcome).toEqual({ kind: "mic-error", reason: "unsupported" });
    expect(screen.getByTestId("phase").textContent).toBe("idle");
    expect(tauri.invoke).not.toHaveBeenCalled();
  });

  it("keeps no half state when the POST fails", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("network"));
    render(
      <>
        <input aria-label="note" />
        <DictationProvider>
          <Probe />
        </DictationProvider>
      </>,
    );
    const field = screen.getByLabelText("note") as HTMLInputElement;
    field.focus();
    await dictate();
    await waitFor(() => expect(latest.outcome?.kind).toBe("refused"));
    expect(screen.getByTestId("phase").textContent).toBe("idle");
    expect(latest.delivery).toBeNull();
    expect(field.value).toBe("");
    expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
    expect(latest.activeKind).toBeNull();

    // The next attempt works and does not reuse the old target.
    daemon.apiFetch.mockResolvedValue({ id: 2, text: "again" });
    (document.activeElement as HTMLElement).blur();
    await dictate();
    await waitFor(() => expect(navigator.clipboard.writeText).toHaveBeenCalledWith("again"));
    expect(field.value).toBe("");
  });

  it("a_button_started_dictation_writes_to_the_field_that_had_focus", async () => {
    render(
      <>
        <input aria-label="note" />
        <button type="button" onMouseDown={keepFocus}>
          mic
        </button>
        <DictationProvider>
          <Probe />
        </DictationProvider>
      </>,
    );
    const field = screen.getByLabelText("note") as HTMLInputElement;
    field.focus();
    // A prevented mousedown is what keeps the browser from moving focus to the button.
    const proceeded = fireEvent.mouseDown(screen.getByText("mic"));
    expect(proceeded).toBe(false);
    await dictate();
    await waitFor(() => expect(field.value).toBe("hello laptop"));
    expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
  });

  it("never_dictates_into_a_password_field", async () => {
    render(
      <>
        <input aria-label="secret" type="password" />
        <DictationProvider>
          <Probe />
        </DictationProvider>
      </>,
    );
    const field = screen.getByLabelText("secret") as HTMLInputElement;
    field.focus();
    await dictate();
    await waitFor(() => expect(navigator.clipboard.writeText).toHaveBeenCalledWith("hello laptop"));
    expect(field.value).toBe("");
  });

  it("the_page_shortcut_drops_the_windows_key", () => {
    const ev = new KeyboardEvent("keydown", { key: "d", ctrlKey: true, altKey: true });
    expect(pageChord("Ctrl+Alt+Super+D")).toBe("Ctrl+Alt+D");
    expect(chordMatches(ev, pageChord("Ctrl+Alt+Super+D"))).toBe(true);
    expect(pageChord("CommandOrControl+Shift+M")).toBe("CommandOrControl+Shift+M");
  });

  it("stops_the_microphone_tracks_on_pagehide", async () => {
    const stop = vi.fn();
    stubMicrophone(() => Promise.resolve({ getTracks: () => [{ stop }] } as unknown as MediaStream));
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await act(async () => {
      await latest.press(false);
    });
    expect(screen.getByTestId("phase").textContent).toBe("recording");
    window.dispatchEvent(new Event("pagehide"));
    expect(stop).toHaveBeenCalled();
  });

  it("matches in-page chords written the way voice.yaml writes them", () => {
    const ev = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
    expect(chordMatches(ev({ key: "d", ctrlKey: true, altKey: true }), "Ctrl+Alt+D")).toBe(true);
    expect(chordMatches(ev({ key: "d", ctrlKey: true }), "Ctrl+Alt+D")).toBe(false);
    expect(chordMatches(ev({ key: " ", code: "Space", ctrlKey: true }), "Ctrl+Space")).toBe(true);
    expect(chordMatches(ev({ key: "d", metaKey: true }), "CommandOrControl+D")).toBe(true);
    expect(chordMatches(ev({ key: "d" }), "")).toBe(false);
  });
});
