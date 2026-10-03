import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { PlusMenu, fileAsContext } from "./PlusMenu";
import { createAppQueryClient } from "../app/queryClient";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/assistant/chats") {
      return [
        {
          chat_id: "c-1",
          cwd: "C:/work/own",
          extra_dirs: [],
          denied_tools: [],
        },
      ];
    }
    if (path === "/assistant/ide-sessions") {
      return [{ session_id: "s-1", cwd: "C:/work/other" }];
    }
    return undefined;
  });
});

function renderMenu(chatId: string | null, handlers: Partial<Parameters<typeof PlusMenu>[0]> = {}) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <PlusMenu
        chatId={chatId}
        onPictures={vi.fn()}
        onText={vi.fn()}
        onMention={vi.fn()}
        {...handlers}
      />
    </QueryClientProvider>,
  );
}

async function open() {
  const trigger = await screen.findByRole("button", { name: "Add to message" });
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.click(trigger);
}

describe("PlusMenu", () => {
  it("offers upload, add context and browse the web", async () => {
    renderMenu("c-1");
    await open();

    expect(await screen.findByRole("menuitem", { name: /Upload from computer/ })).toBeDefined();
    expect(await screen.findByRole("menuitem", { name: /Add context/ })).toBeDefined();
    const web = await screen.findByRole("menuitemcheckbox", { name: /Browse the web/ });
    // Nothing denied yet, so the web is on.
    expect(web.getAttribute("aria-checked")).toBe("true");

    fireEvent.click(web);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ denied_tools: ["WebSearch", "WebFetch"] }),
      });
    });
  });

  it("disables context and the web with no conversation, and says why", async () => {
    renderMenu(null);
    await open();

    const web = await screen.findByRole("menuitemcheckbox", { name: /Browse the web/ });
    expect(web.getAttribute("aria-disabled")).toBe("true");
    expect((await screen.findAllByText(/open a conversation first/)).length).toBe(2);
    expect(
      (await screen.findByRole("menuitem", { name: /Upload from computer/ })).getAttribute(
        "aria-disabled",
      ),
    ).toBeNull();
  });

  it("sends pictures and text apart, and refuses the rest with a note", async () => {
    const onPictures = vi.fn();
    const onText = vi.fn();
    const { container } = renderMenu("c-1", { onPictures, onText });
    const input = container.querySelector('input[type="file"]') as HTMLInputElement;

    const picture = new File(["x"], "a.png", { type: "image/png" });
    const text = new File(["hello"], "notes.md", { type: "text/markdown" });
    const blob = new File([new Uint8Array([0, 1, 2])], "a.bin", { type: "application/octet-stream" });
    fireEvent.change(input, { target: { files: [picture, text, blob] } });

    await waitFor(() => expect(onText).toHaveBeenCalledWith(fileAsContext("notes.md", "hello")));
    expect(onPictures).toHaveBeenCalledWith([picture]);
    expect(await screen.findByText(/only pictures and text files can be attached/)).toBeDefined();
  });

  it("fences a file with a fence longer than any run of backticks inside it", () => {
    expect(fileAsContext("a.txt", "hi")).toBe("```a.txt\nhi\n```\n");
    expect(fileAsContext("a.md", "x ``` y")).toBe("````a.md\nx ``` y\n````\n");
  });
});
