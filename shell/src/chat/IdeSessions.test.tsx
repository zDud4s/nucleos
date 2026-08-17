import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import IdeSessions from "./IdeSessions";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function daemon(sessions: unknown[]) {
  return async () => ({ ok: true, status: 200, json: async () => sessions });
}

async function settle() {
  await act(async () => {});
}

describe("IdeSessions", () => {
  afterEach(() => fetchMock.mockReset());

  it("offers what was said in each one, and where", async () => {
    fetchMock.mockImplementation(
      daemon([
        {
          session_id: "aaaa",
          cwd: "C:\\Projects\\nucleos-canvas",
          title: "arranja o parser de datas",
          last_activity: "2026-08-12T00:00:00+00:00",
        },
      ]),
    );
    render(<IdeSessions token="t" onContinue={() => {}} onClose={() => {}} />);
    await settle();

    expect(screen.getByText("arranja o parser de datas")).toBeTruthy();
    // The project and not the whole path: the column is narrow and the end is the part that names it.
    expect(screen.getByText("nucleos-canvas")).toBeTruthy();
  });

  /**
   * The ID and nothing else. The directory a conversation runs in decides what its turns may touch,
   * so it is looked up by the daemon from the transcript — a window that could name it would be a
   * window that could grant it.
   */
  it("hands back only the id", async () => {
    const onContinue = vi.fn();
    fetchMock.mockImplementation(
      daemon([
        {
          session_id: "aaaa",
          cwd: "C:\\Projects\\nucleos",
          title: "olá",
          last_activity: "2026-08-12T00:00:00+00:00",
        },
      ]),
    );
    render(<IdeSessions token="t" onContinue={onContinue} onClose={() => {}} />);
    await settle();

    await act(async () => {
      fireEvent.click(screen.getByText("olá"));
    });

    expect(onContinue).toHaveBeenCalledWith("aaaa");
    expect(onContinue).toHaveBeenCalledTimes(1);
  });

  it("teaches rather than showing an empty column when there is nothing to pick up", async () => {
    fetchMock.mockImplementation(daemon([]));
    render(<IdeSessions token="t" onContinue={() => {}} onClose={() => {}} />);
    await settle();

    expect(screen.getByText(/Nothing to pick up/i)).toBeTruthy();
  });

  /// A session nobody typed a word into still has to be pickable — it is a conversation, it just
  /// has no name yet, and a blank row would look like a broken one.
  it("still offers a session that was never spoken in", async () => {
    fetchMock.mockImplementation(
      daemon([
        {
          session_id: "aaaa",
          cwd: "C:\\Projects\\nucleos",
          title: null,
          last_activity: "2026-08-12T00:00:00+00:00",
        },
      ]),
    );
    render(<IdeSessions token="t" onContinue={() => {}} onClose={() => {}} />);
    await settle();

    expect(screen.getByText("Nothing said yet")).toBeTruthy();
  });
});
