import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

/* The audio graph and the route, which are the two things jsdom cannot have: it has no
   `AudioContext`, no `getUserMedia`, and no daemon. What is left under test is the whole of what
   this hook decides. */
const capture = vi.hoisted(() => ({ startCapture: vi.fn(), finishCapture: vi.fn() }));
vi.mock("../lib/capture", () => capture);

const voice = vi.hoisted(() => ({ postCapture: vi.fn() }));
vi.mock("./voice", async (original) => ({
  ...(await original<typeof import("./voice")>()),
  ...voice,
}));

import { useDictation } from "./dictation";

/** A microphone that opens and closes without ever touching an audio API. */
function anOpenMicrophone() {
  const opened = { frames: [] } as unknown as Awaited<ReturnType<typeof capture.startCapture>>;
  capture.startCapture.mockResolvedValue(opened);
  capture.finishCapture.mockResolvedValue({ bytes: new Uint8Array([1]), ms: 900 });
  return opened;
}

beforeEach(() => {
  capture.startCapture.mockReset();
  capture.finishCapture.mockReset();
  voice.postCapture.mockReset();
});

describe("useDictation", () => {
  it("hands back what was heard, and is off again afterwards", async () => {
    anOpenMicrophone();
    voice.postCapture.mockResolvedValue({ text: "olá mundo" });
    const heard: string[] = [];
    const { result } = renderHook(() => useDictation((said) => heard.push(said)));

    expect(result.current.phase).toBe("off");

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("off"));

    expect(heard).toEqual(["olá mundo"]);
    expect(voice.postCapture).toHaveBeenCalledWith(new Uint8Array([1]), "dictation", 900);
    expect(result.current.trouble).toBeNull();
  });

  /* 204 is the one success-family status in this shell that is a negative answer. Saying nothing
     into an open microphone is ordinary, so it must not put text in the box and must not read as
     a fault. */
  it("says nothing was heard rather than writing an empty sentence into the box", async () => {
    anOpenMicrophone();
    voice.postCapture.mockResolvedValue(undefined);
    const heard: string[] = [];
    const { result } = renderHook(() => useDictation((said) => heard.push(said)));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));
    act(() => result.current.toggle());

    await waitFor(() => expect(result.current.trouble).toBe("nothing was heard"));
    expect(heard).toEqual([]);
    expect(result.current.phase).toBe("off");
  });

  it("says so when the machine has no microphone, and stays off", async () => {
    capture.startCapture.mockRejectedValue(new Error("NotAllowedError"));
    const { result } = renderHook(() => useDictation(() => {}));

    act(() => result.current.toggle());

    await waitFor(() => expect(result.current.trouble).toMatch(/no microphone/));
    expect(result.current.phase).toBe("off");
  });

  /* On the first ever press the await is a PERMISSION PROMPT, which somebody can leave standing.
     A second press during it used to take the "nothing is recording" branch and open a second
     device — two microphones, and only one of them ever closed. */
  it("opens one device however many times it is pressed while the prompt is up", async () => {
    let allow: (value: unknown) => void = () => {};
    capture.startCapture.mockReturnValue(
      new Promise((resolve) => {
        allow = resolve;
      }),
    );
    const { result } = renderHook(() => useDictation(() => {}));

    act(() => result.current.toggle());
    act(() => result.current.toggle());
    act(() => result.current.toggle());
    expect(result.current.phase).toBe("off");

    await act(async () => {
      allow({ frames: [] });
    });

    expect(capture.startCapture).toHaveBeenCalledTimes(1);
    expect(result.current.phase).toBe("listening");
  });

  /* An open microphone must not survive the box it belongs to. On the front door, unmounting is
     exactly what happens when the first message opens a conversation — so a graph left up there
     would leave the device light on for the rest of the session. */
  it("closes the device when the box it belongs to goes away", async () => {
    const opened = anOpenMicrophone();
    const { result, unmount } = renderHook(() => useDictation(() => {}));

    act(() => result.current.toggle());
    await waitFor(() => expect(result.current.phase).toBe("listening"));

    unmount();

    expect(capture.finishCapture).toHaveBeenCalledWith(opened);
  });
});
