import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Term } from "./Term";
import { TERMS } from "../copy";

describe("Term", () => {
  it("shows the word and keeps its definition closed", () => {
    render(<Term term="shadow" />);
    expect(screen.getByText("shadow")).toBeTruthy();
    expect(screen.queryByText(TERMS.shadow)).toBeNull();
  });

  it("opens the glossary definition on focus and describes the word by it", () => {
    render(<Term term="gate" />);
    const word = screen.getByText("gate");
    fireEvent.focus(word);
    const tip = screen.getByText(TERMS.gate);
    expect(tip).toBeTruthy();
    const id = word.getAttribute("aria-describedby");
    expect(id).toBeTruthy();
    expect(document.getElementById(id as string)?.textContent).toContain(TERMS.gate);
  });

  it("lets the page word the term its own way", () => {
    render(<Term term="ceiling">limit</Term>);
    expect(screen.getByText("limit")).toBeTruthy();
    fireEvent.focus(screen.getByText("limit"));
    expect(screen.getByText(TERMS.ceiling)).toBeTruthy();
  });
});
