import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { NoteText } from "./NoteText";

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

  it("opens links safely", () => {
    const { container } = render(<NoteText text="[site](https://example.com)" />);
    const link = container.querySelector("a");
    expect(link?.getAttribute("href")).toBe("https://example.com");
    expect(link?.getAttribute("target")).toBe("_blank");
    expect(link?.getAttribute("rel")).toBe("noreferrer noopener");
  });

  it("drops a javascript: link target", () => {
    const { container } = render(<NoteText text="[bad](javascript:alert(1))" />);
    expect(container.querySelector("a")?.getAttribute("href") ?? "").not.toContain("javascript");
  });

  it("emits no style attribute", () => {
    const { container } = render(
      <NoteText text={'# T\n\n- a\n\n[l](https://example.com) <span style="color:red">x</span>'} />,
    );
    expect(container.querySelector("[style]")).toBeNull();
  });
});
