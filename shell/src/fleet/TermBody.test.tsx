import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { TermBody } from "./TermBody";
import { line } from "./test-fixtures";

afterEach(cleanup);

describe("TermBody", () => {
  it("is a silent terminal: aria-live is off", () => {
    render(<TermBody lines={[line("text", "hi")]} label="nucleos-37" card={false} />);
    const term = screen.getByRole("group", { name: "Terminal of nucleos-37" });
    expect(term.getAttribute("aria-live")).toBe("off");
  });

  it("shows the newest twelve lines, oldest first, one class per kind", () => {
    const lines = Array.from({ length: 15 }, (_, i) => line(i % 2 === 0 ? "tool" : "result", `l${i}`));
    const { container } = render(<TermBody lines={lines} label="x" card={false} />);
    const rows = [...container.querySelectorAll(".fleet-ln")];
    expect(rows.map((r) => r.textContent)).toEqual(lines.slice(-12).map((l) => l.text));
    expect(rows[0].className).toContain("fleet-ln-result"); // l3
    expect(rows[1].className).toContain("fleet-ln-tool"); // l4
  });

  it("renders every line as plain text, never as markup", () => {
    const hostile = '<img src=x onerror="alert(1)"><script>alert(2)</script>';
    const { container } = render(<TermBody lines={[line("text", hostile)]} label="x" card={false} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector(".fleet-ln")?.textContent).toBe(hostile);
  });

  it("in card mode shows only the current line", () => {
    const { container } = render(<TermBody lines={[line("prompt", "a"), line("text", "b")]} label="x" card />);
    expect([...container.querySelectorAll(".fleet-ln")].map((r) => r.textContent)).toEqual(["b"]);
    expect(container.querySelector(".fleet-term-card")).not.toBeNull();
  });
});
