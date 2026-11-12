import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { forgetSeat, postAnswer, postInput, rememberSeat, seatNonce, useSeatNonce } from "./seat";
import type { InputEvent, PromptAnswer } from "./liveRecords";

beforeEach(() => {
  cleanup();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({});
  forgetSeat(7);
  forgetSeat(8);
});

describe("seat store", () => {
  it("remembers a seat nonce per session and forgets it", () => {
    expect(seatNonce(7)).toBeUndefined();
    rememberSeat(7, "n-seven");
    rememberSeat(8, "n-eight");
    expect(seatNonce(7)).toBe("n-seven");
    expect(seatNonce(8)).toBe("n-eight");

    const { result } = renderHook(() => useSeatNonce(7));
    expect(result.current).toBe("n-seven");

    act(() => forgetSeat(7));
    expect(seatNonce(7)).toBeUndefined();
    expect(seatNonce(8)).toBe("n-eight");
    expect(result.current).toBeUndefined();
  });

  it("postInput and postAnswer send the nonce to the core routes", async () => {
    const events = [{ type: "click", x: 1, y: 2 }] as unknown as InputEvent[];
    await postInput(7, "n-seven", events);
    expect(daemon.apiFetch).toHaveBeenCalledTimes(1);
    const [inputPath, inputInit] = daemon.apiFetch.mock.calls[0];
    expect(inputPath).toBe("/browser/sessions/7/input");
    expect(inputInit.method).toBe("POST");
    expect(JSON.parse(inputInit.body)).toEqual({ seat_nonce: "n-seven", events });

    const answer = { kind: "confirm", accept: true } as unknown as PromptAnswer;
    await postAnswer(7, "n-seven", "3", answer);
    expect(daemon.apiFetch).toHaveBeenCalledTimes(2);
    const [answerPath, answerInit] = daemon.apiFetch.mock.calls[1];
    expect(answerPath).toBe("/browser/sessions/7/answer");
    expect(answerInit.method).toBe("POST");
    expect(JSON.parse(answerInit.body)).toEqual({ seat_nonce: "n-seven", prompt: "3", answer });
  });
});
