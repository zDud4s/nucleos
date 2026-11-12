import { act, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => tauri);

import { renderWithRouter } from "../test/harness";
import { CONVERSATION_TOGGLE_EVENT, ConversationChord } from "./ConversationChord";

let chord: (() => void) | null;

beforeEach(() => {
  chord = null;
  tauri.listen.mockReset();
  tauri.listen.mockImplementation(async (event: string, handler: () => void) => {
    if (event === CONVERSATION_TOGGLE_EVENT) chord = handler;
    return () => {};
  });
});

describe("ConversationChord", () => {
  /* The whole defect this component exists for: the chord is global at the operating system and was
     answered by one page. Pressed on any other, it did nothing and said nothing. */
  it("takes the person to the Voice page from any other page, asking it to talk", async () => {
    const { router } = await renderWithRouter(<ConversationChord />, { initialPath: "/chats" });
    await waitFor(() => expect(chord).not.toBeNull());

    act(() => chord?.());

    await waitFor(() => expect(router.state.location.pathname).toBe("/voice"));
    expect(typeof (router.state.location.search as { talk?: unknown }).talk).toBe("number");
  });

  /* Two presses are two requests, even when the page is already showing: a new stamp is what makes
     the second one a navigation the page can see, rather than the same address visited twice. */
  it("gives every press a stamp of its own", async () => {
    const now = vi.spyOn(Date, "now");
    const { router } = await renderWithRouter(<ConversationChord />, { initialPath: "/voice" });
    await waitFor(() => expect(chord).not.toBeNull());

    now.mockReturnValue(1);
    act(() => chord?.());
    await waitFor(() => expect(router.state.location.search).toEqual({ talk: 1 }));
    now.mockReturnValue(2);
    act(() => chord?.());
    await waitFor(() => expect(router.state.location.search).toEqual({ talk: 2 }));
    now.mockRestore();
  });

  it("stays quiet where there is no host to listen to", async () => {
    tauri.listen.mockRejectedValue(new Error("no Tauri runtime"));

    const { router } = await renderWithRouter(<ConversationChord />, { initialPath: "/chats" });

    await waitFor(() => expect(tauri.listen).toHaveBeenCalled());
    expect(router.state.location.pathname).toBe("/chats");
  });
});
