import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ChannelCapabilities } from "../data/channels";
import { CapabilityList } from "./CapabilityList";

const telegram: ChannelCapabilities = {
  buttons: true, edit: true, typing: true, threads: true, reply_to: true,
  max_text: 4096, media_in: ["audio", "image", "file"], media_out: ["image", "file", "audio"],
};

describe("CapabilityList", () => {
  it("reports nothing missing for full capabilities", () => {
    render(<CapabilityList capabilities={telegram} />);
    expect(screen.getByText(/all capabilities present/i)).toBeTruthy();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });

  it("lists buttons and threads for a discord-like channel", () => {
    render(<CapabilityList capabilities={{ ...telegram, buttons: false, threads: false }} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByText(/reply "approve 12" \/ "reject 12"/)).toBeTruthy();
    expect(screen.getByText(/falls back to the home conversation with a \[project\] prefix/)).toBeTruthy();
  });

  it("explains reply_to, typing and edit", () => {
    render(<CapabilityList capabilities={{ ...telegram, reply_to: false, typing: false, edit: false }} />);
    expect(screen.getByText(/\/cap <N> <text>/)).toBeTruthy();
    expect(screen.getAllByText(/not used/)).toHaveLength(2);
  });

  it("names the missing media_out type", () => {
    render(<CapabilityList capabilities={{ ...telegram, media_out: ["image", "file"] }} />);
    expect(screen.getByText(/audio/)).toBeTruthy();
    expect(screen.getByText(/that part is replaced by text: file available in NucleOS: <name>/)).toBeTruthy();
  });

  it("handles unknown capabilities", () => {
    render(<CapabilityList capabilities={null} />);
    expect(screen.getByText(/capabilities are not known yet: the adapter has not said hello since the daemon started/i)).toBeTruthy();
  });

  it("never uses portuguese command words", () => {
    const { container } = render(
      <CapabilityList capabilities={{ ...telegram, buttons: false, threads: false, reply_to: false, typing: false, edit: false, media_out: [] }} />,
    );
    expect(container.textContent).not.toMatch(/aprovar|rejeitar/i);
  });
});
