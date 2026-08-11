import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import ChatList from "./ChatList";
import type { ChatRow } from "../api";

function chat(overrides: Partial<ChatRow> & { chat_id: string }): ChatRow {
  return {
    title: null,
    brain: "cloud",
    created_at: "2026-08-11T10:00:00+00:00",
    first_message: null,
    last_activity: null,
    waiting: 0,
    ...overrides,
  };
}

const noop = () => {};

describe("ChatList", () => {
  it("falls back to the first message when nobody named the conversation", () => {
    render(
      <ChatList
        chats={[chat({ chat_id: "a", first_message: "quanto sobra este mês?" })]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getByText("quanto sobra este mês?")).toBeTruthy();
  });

  it("prefers a name somebody wrote over the first message", () => {
    render(
      <ChatList
        chats={[chat({ chat_id: "a", title: "o orçamento", first_message: "quanto sobra?" })]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getByText("o orçamento")).toBeTruthy();
    expect(screen.queryByText("quanto sobra?")).toBeNull();
  });

  it("says so when a conversation has not been used at all", () => {
    // The whole reason the daemon keeps a table: a chat can exist before it has said anything.
    render(
      <ChatList
        chats={[chat({ chat_id: "a" })]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getByText(/nothing said yet/i)).toBeTruthy();
  });

  it("marks only the conversation that is thinking", () => {
    // The daemon holds one turn slot PER CHAT, so two conversations can think at once. A mark on
    // all of them would be the window lying about what the daemon is doing.
    render(
      <ChatList
        chats={[chat({ chat_id: "a" }), chat({ chat_id: "b" })]}
        selected={null}
        busy={new Set(["a"])}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getAllByText("thinking…")).toHaveLength(1);
  });

  it("says which conversations answered while you were elsewhere", () => {
    render(
      <ChatList
        chats={[
          chat({ chat_id: "answered", title: "the one that answered", waiting: 2 }),
          chat({ chat_id: "quiet", title: "the quiet one" }),
        ]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getAllByText(/answered/i).length).toBeGreaterThan(0);
    // The count, not just a dot: two answers landed and the number says how much there is to read.
    expect(screen.getByText("2")).toBeTruthy();
    expect(screen.getByLabelText(/2 answers waiting/i)).toBeTruthy();
  });

  it("says nothing about a conversation with nothing new in it", () => {
    render(
      <ChatList
        chats={[chat({ chat_id: "quiet", title: "the quiet one" })]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.queryByLabelText(/waiting/i)).toBeNull();
  });

  it("does not call a conversation waiting while it is still being answered", () => {
    // The two are different states and they can be true at once — the daemon counts only turns that
    // LANDED, so a chat mid-turn with an older unread answer says both, and each says its own thing.
    render(
      <ChatList
        chats={[chat({ chat_id: "a", title: "mid-turn", waiting: 0 })]}
        selected={null}
        busy={new Set(["a"])}
        onSelect={noop}
        onNew={noop}
        onArchive={noop}
      />,
    );

    expect(screen.getByText("thinking…")).toBeTruthy();
    expect(screen.queryByLabelText(/waiting/i)).toBeNull();
  });

  it("opens a new conversation on request", () => {
    const onNew = vi.fn();
    render(
      <ChatList
        chats={[]}
        selected={null}
        busy={new Set()}
        onSelect={noop}
        onNew={onNew}
        onArchive={noop}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /new conversation/i }));

    expect(onNew).toHaveBeenCalled();
  });

  it("selects the conversation you click", () => {
    const onSelect = vi.fn();
    render(
      <ChatList
        chats={[chat({ chat_id: "a", title: "o orçamento" })]}
        selected={null}
        busy={new Set()}
        onSelect={onSelect}
        onNew={noop}
        onArchive={noop}
      />,
    );

    fireEvent.click(screen.getByText("o orçamento"));

    expect(onSelect).toHaveBeenCalledWith("a");
  });
});
