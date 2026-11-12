// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const parsing = vi.hoisted(() => ({ blocks: vi.fn() }));
vi.mock("../lib/rich", async (original) => {
  const real = await original<typeof import("../lib/rich")>();
  parsing.blocks.mockImplementation(real.blocks);
  return { ...real, blocks: parsing.blocks };
});

import { MessageList, Rich } from "./MessageList";

describe("MessageList", () => {
  it("MessageList draws the person's text verbatim and the agent's through Rich", () => {
    const { container } = render(
      <MessageList
        messages={[
          { key: "a", role: "person", text: "keep **this** literal" },
          { key: "b", role: "agent", text: "```\nlet x = 1;\n```" },
        ]}
      />,
    );

    // The person's half is drawn exactly as typed: the asterisks survive.
    expect(screen.getByText("keep **this** literal")).toBeTruthy();
    // The agent's half goes through Rich: a fenced block becomes a code block.
    const code = container.querySelector(".chats-code-block code");
    expect(code).not.toBeNull();
    expect(code?.textContent).toContain("let x = 1;");
  });

  it("Rich parses a text once across re-renders", () => {
    parsing.blocks.mockClear();
    const { rerender } = render(<Rich text="some **answer**" />);
    expect(parsing.blocks).toHaveBeenCalledTimes(1);

    rerender(<Rich text="some **answer**" />);
    rerender(<Rich text="some **answer**" />);
    expect(parsing.blocks).toHaveBeenCalledTimes(1);

    rerender(<Rich text="another answer" />);
    expect(parsing.blocks).toHaveBeenCalledTimes(2);
  });
});
