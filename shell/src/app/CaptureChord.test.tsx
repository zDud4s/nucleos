import { act, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => tauri);

import { renderWithRouter } from "../test/harness";
import { CAPTURE_EVENT, CaptureChord } from "./CaptureChord";

let chord: (() => void) | null;

beforeEach(() => {
  chord = null;
  tauri.listen.mockReset();
  tauri.listen.mockImplementation(async (event: string, handler: () => void) => {
    if (event === CAPTURE_EVENT) chord = handler;
    return () => {};
  });
});

describe("CaptureChord", () => {
  it("the capture chord opens the Brain with a capture stamp", async () => {
    const { router } = await renderWithRouter(<CaptureChord />, { initialPath: "/chats" });
    await waitFor(() => expect(chord).not.toBeNull());

    act(() => chord?.());

    await waitFor(() => expect(router.state.location.pathname).toBe("/brain"));
    expect(typeof (router.state.location.search as { capture?: unknown }).capture).toBe("number");
  });

  it("gives every press a stamp of its own", async () => {
    const now = vi.spyOn(Date, "now");
    const { router } = await renderWithRouter(<CaptureChord />, { initialPath: "/brain" });
    await waitFor(() => expect(chord).not.toBeNull());

    now.mockReturnValue(1);
    act(() => chord?.());
    await waitFor(() => expect(router.state.location.search).toMatchObject({ capture: 1 }));
    now.mockReturnValue(2);
    act(() => chord?.());
    await waitFor(() => expect(router.state.location.search).toMatchObject({ capture: 2 }));
    now.mockRestore();
  });

  it("stays quiet where there is no host to listen to", async () => {
    tauri.listen.mockRejectedValue(new Error("no Tauri runtime"));

    const { router } = await renderWithRouter(<CaptureChord />, { initialPath: "/chats" });

    await waitFor(() => expect(tauri.listen).toHaveBeenCalled());
    expect(router.state.location.pathname).toBe("/chats");
  });
});
