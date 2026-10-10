import { describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { Panel } from "./Panel";
import type { Incoming, Outgoing } from "./protocol";

/**
 * The panel is two props wide: `send` for what the person does, `subscribe`
 * for what the sidecar pushes. `subscribe(fn)` hands `fn` every incoming
 * message and returns the unsubscribe.
 */
function mount() {
  const sent: Outgoing[] = [];
  let push: (m: Incoming) => void = () => {};
  const send = vi.fn((m: Outgoing) => {
    sent.push(m);
  });
  const subscribe = (fn: (m: Incoming) => void) => {
    push = fn;
    return () => {};
  };
  render(<Panel send={send} subscribe={subscribe} />);
  const deliver = (m: Incoming) => act(() => push(m));
  return { sent, deliver };
}

describe("Panel main button", () => {
  it("Panel main button takes the wheel in agent mode and hands back with the note in person mode", () => {
    const { sent, deliver } = mount();
    deliver({ v: 1, kind: "state", mode: "agent", host: "example.org", collapsed: false });

    expect(screen.getByText("The agent is driving")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Take the wheel" }));
    expect(sent).toContainEqual({ v: 1, kind: "take_wheel" });

    deliver({ v: 1, kind: "state", mode: "human", host: "example.org", collapsed: false });
    expect(screen.getByText("You are driving")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Take the wheel" })).toBeNull();

    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "I logged in, carry on" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Hand back to the agent" }));
    expect(sent).toContainEqual({ v: 1, kind: "give_back", note: "I logged in, carry on" });
  });
});

describe("Panel ask_keep", () => {
  it("Panel ask_keep answers keep with writable", () => {
    const { sent, deliver } = mount();
    deliver({ v: 1, kind: "state", mode: "agent", host: "example.org", collapsed: false });
    deliver({ v: 1, kind: "ask_keep", hosts: ["https://example.org"] });

    expect(screen.getByText("May the agent use these sites?")).toBeTruthy();
    fireEvent.click(screen.getByRole("checkbox", { name: /also submit forms/i }));
    fireEvent.click(screen.getByRole("button", { name: /^yes$/i }));

    expect(sent).toContainEqual({ v: 1, kind: "keep", keep: true, writable: true });
  });
});
