import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { NoteText } from "./NoteText";

const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);

beforeEach(() => {
  opener.openUrl.mockReset();
  opener.openUrl.mockResolvedValue(undefined);
});

describe("NoteText", () => {
  it("renders headings, lists and code", () => {
    const { container } = render(
      <NoteText text={"# Title\n\n- one\n- two\n\nsome `inline` code\n\n```\nblock\n```"} />,
    );
    expect(container.querySelector("h1")?.textContent).toBe("Title");
    expect(container.querySelectorAll("ul > li")).toHaveLength(2);
    expect(container.querySelector("p code")?.textContent).toBe("inline");
    expect(container.querySelector("pre code")?.textContent).toContain("block");
    expect(container.querySelector(".brain-text.brain-markdown")).not.toBeNull();
  });

  it("never renders raw HTML as elements", () => {
    const { container } = render(
      <NoteText text={'before <script>alert(1)</script> <img src="x" onerror="alert(2)"> <b>bold</b> after'} />,
    );
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("b")).toBeNull();
    expect(container.innerHTML).not.toContain("onerror");
  });

  it("does not render markdown images", () => {
    const { container } = render(<NoteText text="![pic](https://example.com/a.png)" />);
    expect(container.querySelector("img")).toBeNull();
  });

  it("keeps the alt text of an image-only note", () => {
    const { container } = render(<NoteText text="![a diagram](https://example.com/a.png)" />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("a diagram");
  });

  it("renders a link as a button that hands the URL to the opener, never as an href", () => {
    const { container } = render(<NoteText text="[site](https://example.com)" />);
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("[href]")).toBeNull();
    const button = container.querySelector("button");
    expect(button?.textContent).toBe("site");
    expect(button?.getAttribute("title")).toBe("https://example.com");
    fireEvent.click(button as HTMLButtonElement);
    expect(opener.openUrl).toHaveBeenCalledWith("https://example.com");
  });

  it("renders a link with a stripped javascript: target as plain text", () => {
    const { container } = render(<NoteText text="[bad](javascript:alert(1))" />);
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("button")).toBeNull();
    expect(container.textContent).toContain("bad");
    expect(container.innerHTML).not.toContain("javascript");
  });

  it("does not open a relative link", () => {
    const { container } = render(<NoteText text="[rel](/some/path)" />);
    expect(container.querySelector("button")).toBeNull();
    expect(container.textContent).toContain("rel");
  });

  it("emits no style attribute", () => {
    const { container } = render(
      <NoteText text={'# T\n\n- a\n\n[l](https://example.com) <span style="color:red">x</span>'} />,
    );
    expect(container.querySelector("[style]")).toBeNull();
  });
});
