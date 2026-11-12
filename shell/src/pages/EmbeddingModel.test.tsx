import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { EmbeddingModel } from "./EmbeddingModel";
import { renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function saveButton() {
  return screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
}

describe("EmbeddingModel", () => {
  it("shows the stored model and offers Save only for a different name", async () => {
    daemon.apiFetch.mockResolvedValue({ model: "nomic-embed-text" });
    renderWithQuery(<EmbeddingModel />);

    const input = (await screen.findByLabelText("Embedding model")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("nomic-embed-text"));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/embedding");
    expect(saveButton().disabled).toBe(true);

    fireEvent.change(input, { target: { value: "mxbai-embed-large" } });
    expect(saveButton().disabled).toBe(false);
  });

  it("saving posts the trimmed name to the daemon", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) =>
      init?.method === "POST" ? { model: "mxbai-embed-large" } : { model: "nomic-embed-text" },
    );
    renderWithQuery(<EmbeddingModel />);

    const input = (await screen.findByLabelText("Embedding model")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("nomic-embed-text"));
    fireEvent.change(input, { target: { value: "  mxbai-embed-large " } });
    fireEvent.click(saveButton());

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/embedding", {
        method: "POST",
        body: JSON.stringify({ model: "mxbai-embed-large" }),
      }),
    );
  });
});
