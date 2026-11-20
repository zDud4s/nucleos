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

async function expectHidden(answer: { state: string; banner: string | null }) {
  daemon.apiFetch.mockResolvedValue(answer);
  const { container } = renderWithQuery(<SandboxBanner />);
  await vi.waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
  await Promise.resolve();
  expect(container.textContent).toBe("");
}

describe("SandboxBanner", () => {
  it("is hidden when there is no backend", async () => {
    await expectHidden({ state: "no_backend", banner: "sandbox por implementar" });
  });

  it("is hidden when off", async () => {
    await expectHidden({ state: "off", banner: "sandbox off" });
  });

  it("is hidden when armed", async () => {
    await expectHidden({ state: "armed", banner: null });
  });

  it("is shown when refused", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "refused", banner: "sandbox refused: runs blocked" });
    renderWithQuery(<SandboxBanner />);
    expect(await screen.findByText("sandbox refused: runs blocked")).toBeTruthy();
  });
});
