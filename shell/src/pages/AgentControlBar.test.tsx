// §spec pilar-de-browser
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApiRefusal } from "../data/client";
import { newTab } from "../data/browser-tabs";
import { AgentControlBar, resetAgentControlState } from "./AgentControlBar";

const mocks = vi.hoisted(() => ({ embeddedOwner: vi.fn(), embeddedKeep: vi.fn() }));
vi.mock("../data/browser-embedded", () => ({
  embeddedOwner: mocks.embeddedOwner,
  embeddedKeep: mocks.embeddedKeep,
}));

const tab = { ...newTab("t1", "https://a.example/", "agent"), sessionId: "s1" };

function bar(over: Partial<{ highlight: boolean; onOpen: () => void; sessionless: boolean }> = {}) {
  const t = over.sessionless ? { ...tab, sessionId: undefined } : tab;
  return (
    <AgentControlBar tab={t} highlight={over.highlight ?? false} onOpenInMyTabs={over.onOpen ?? (() => {})} />
  );
}

async function takeControl() {
  fireEvent.click(screen.getByRole("button", { name: "Take control" }));
  await screen.findByRole("button", { name: "Give back to agent" });
}

beforeEach(() => {
  resetAgentControlState();
  mocks.embeddedOwner.mockReset();
  mocks.embeddedOwner.mockResolvedValue(undefined);
  mocks.embeddedKeep.mockReset();
  mocks.embeddedKeep.mockResolvedValue({ granted: [] });
});

describe("AgentControlBar", () => {
  it("starts in agent mode and takes control", async () => {
    render(bar());
    expect(screen.getByText("Agent is driving")).toBeTruthy();
    await takeControl();
    expect(mocks.embeddedOwner).toHaveBeenCalledWith("s1", "human");
    expect(screen.getByRole("button", { name: "Open in my tabs" })).toBeTruthy();
  });

  it("disables Take control without a session", () => {
    render(bar({ sessionless: true }));
    expect((screen.getByRole("button", { name: "Take control" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("gives back straight to the agent on an empty chain", async () => {
    mocks.embeddedOwner.mockResolvedValueOnce(undefined).mockResolvedValueOnce({ chain: [], row_id: 3 });
    render(bar());
    await takeControl();
    fireEvent.click(screen.getByRole("button", { name: "Give back to agent" }));
    await screen.findByText("Agent is driving");
    expect(mocks.embeddedOwner).toHaveBeenLastCalledWith("s1", "agent");
    expect(screen.queryByText("Keep these sites?")).toBeNull();
  });

  it("asks which sites to keep and posts each answer", async () => {
    const cases: [string, [number, boolean, boolean]][] = [
      ["Keep", [7, true, false]],
      ["Keep, and allow forms", [7, true, true]],
      ["No", [7, false, false]],
    ];
    for (const [label, args] of cases) {
      resetAgentControlState();
      mocks.embeddedKeep.mockClear();
      mocks.embeddedOwner
        .mockResolvedValueOnce(undefined)
        .mockResolvedValueOnce({ chain: ["https://a.example/x", "https://a.example/y", "https://b.example/"], row_id: 7 });
      const { unmount } = render(bar());
      await takeControl();
      fireEvent.click(screen.getByRole("button", { name: "Give back to agent" }));
      await screen.findByText("Keep these sites?");
      expect(screen.getByText("a.example")).toBeTruthy();
      expect(screen.getByText("b.example")).toBeTruthy();
      expect(screen.getAllByText("a.example")).toHaveLength(1);
      fireEvent.click(screen.getByRole("button", { name: label }));
      await screen.findByText("Agent is driving");
      expect(mocks.embeddedKeep).toHaveBeenCalledWith(...args);
      unmount();
    }
  });

  it("shows a quiet notice when the pillar is off", async () => {
    mocks.embeddedOwner.mockRejectedValue(new ApiRefusal(409, "pillar_off", ""));
    render(bar());
    fireEvent.click(screen.getByRole("button", { name: "Take control" }));
    const note = await screen.findByRole("status");
    expect(note.textContent).toBe("Could not take control (pillar is off)");
    expect(screen.getByText("Agent is driving")).toBeTruthy();
  });

  it("shows a notice when the session is gone", async () => {
    mocks.embeddedOwner.mockRejectedValue(new ApiRefusal(404, "not_found", ""));
    render(bar());
    fireEvent.click(screen.getByRole("button", { name: "Take control" }));
    expect((await screen.findByRole("status")).textContent).toBe("Session is gone");
  });

  it("calls back to open the tab in my tabs", async () => {
    const onOpen = vi.fn();
    render(bar({ onOpen }));
    await takeControl();
    fireEvent.click(screen.getByRole("button", { name: "Open in my tabs" }));
    expect(onOpen).toHaveBeenCalledWith(tab);
  });

  it("marks Take control when highlighted", () => {
    render(bar({ highlight: true }));
    expect(screen.getByRole("button", { name: "Take control" }).getAttribute("data-highlight")).toBe("true");
  });

  it("keeps the human state across a remount", async () => {
    const first = render(bar());
    await takeControl();
    first.unmount();
    render(bar());
    await waitFor(() => expect(screen.getByRole("button", { name: "Give back to agent" })).toBeTruthy());
  });
});
