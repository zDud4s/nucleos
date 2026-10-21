import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent } from "@testing-library/react";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { NotchProviders } from "./NotchProviders";
import { readHidden } from "../app/notch-providers";
import { renderWithQuery } from "../test/harness";

function provider(name: string) {
  return { provider: name, fidelity: "official", read_at: new Date().toISOString(), windows: [], detail: "", severity: "" };
}

afterEach(() => window.localStorage.removeItem("nucleos.notch-hidden"));

describe("NotchProviders", () => {
  it("offers one switch per reported subscription and remembers an untick", async () => {
    daemon.apiFetch.mockResolvedValue({ providers: [provider("claude"), provider("codex")], source: "sidecar", cached: false });
    const { findByLabelText } = renderWithQuery(<NotchProviders />);
    const codex = (await findByLabelText("Show codex")) as HTMLInputElement;
    expect(codex.checked).toBe(true);

    fireEvent.click(codex);
    expect(codex.checked).toBe(false);
    expect(readHidden()).toEqual(["codex"]);
  });

  /** Hidden on a day the sidecar no longer reports it, it can still be ticked back on. */
  it("keeps a hidden provider in the list even when it is not reported", async () => {
    window.localStorage.setItem("nucleos.notch-hidden", JSON.stringify(["gemini"]));
    daemon.apiFetch.mockResolvedValue({ providers: [provider("claude")], source: "sidecar", cached: false });
    const { findByLabelText } = renderWithQuery(<NotchProviders />);
    const gemini = (await findByLabelText("Show gemini")) as HTMLInputElement;
    expect(gemini.checked).toBe(false);

    fireEvent.click(gemini);
    expect(readHidden()).toEqual([]);
  });
});
