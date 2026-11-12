import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { SandboxBanner } from "./SandboxBanner";
import { renderWithQuery } from "../test/harness";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => daemon.apiFetch.mockReset());

describe("SandboxBanner", () => {
  it('shows "sandbox por implementar" when no backend', async () => {
    daemon.apiFetch.mockResolvedValue({ state: "no_backend", banner: "sandbox por implementar" });
    renderWithQuery(<SandboxBanner />);
    expect(await screen.findByText("sandbox por implementar")).toBeTruthy();
    expect(screen.queryByText(/armada/i)).toBeNull();
  });

  it("is absent when armed", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "armed", banner: null });
    const { container } = renderWithQuery(<SandboxBanner />);
    await vi.waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    await Promise.resolve();
    expect(container.textContent).toBe("");
  });
});
