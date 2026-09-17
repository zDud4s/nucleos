import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Quiet } from "./Quiet";
import { Section } from "./Section";

describe("Quiet", () => {
  it("says the one thing and keeps the paragraph closed", () => {
    render(
      <Quiet says="none declared">
        A command is a name, something to run, and whether its result is a verdict.
      </Quiet>,
    );

    expect(screen.getByText("none declared")).toBeTruthy();
    // The whole point: the reasoning costs no pixels until it is asked for.
    expect(screen.queryByText(/A command is a name/)).toBeNull();
  });

  it("the question names its section and its answer", () => {
    render(<Quiet says="none declared">Declaration rather than detection.</Quiet>);

    const ask = screen.getByRole("button", { name: "why?" });
    const said = ask.getAttribute("aria-describedby");
    const why = ask.getAttribute("aria-controls");
    expect(said).toBeTruthy();
    expect(screen.getByText("none declared").getAttribute("id")).toBe(said);
    expect(why).toBeTruthy();
    const answer = document.getElementById(why ?? "");
    expect(answer?.className).toContain("ui-quiet-why");
    expect(answer?.getAttribute("hidden")).not.toBeNull();
    expect(answer?.textContent).toBe("");
  });

  it("gives the paragraph back, whole, on one click", () => {
    render(
      <Quiet says="none installed">
        Which is a real answer and not a gap — this project develops however whoever is at the
        keyboard decides.
      </Quiet>,
    );

    const ask = screen.getByRole("button", { name: "why?" });
    expect(ask.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(ask);
    expect(screen.getByText(/a real answer and not a gap/)).toBeTruthy();
    // The same control closes it, and says so — a "why?" that stayed "why?" while
    // the answer was open would be the only affordance for closing it again.
    expect(screen.getByRole("button", { name: "less" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("offers no question at all when there is nothing behind it", () => {
    // A disclosure that opens onto nothing is worse than no disclosure: somebody
    // presses it once and learns this app's controls are decorative.
    render(<Quiet says="no ceiling · nothing in flight" />);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("keeps the gesture that would fill the section in front of you", () => {
    // Not behind the disclosure with the reasoning. What would fill an empty
    // section is the reason anybody is looking at one.
    render(
      <Quiet says="none declared" action={<button type="button">declare a command</button>}>
        Declaration rather than detection.
      </Quiet>,
    );

    expect(screen.getByRole("button", { name: "declare a command" })).toBeTruthy();
  });

  it("shares its heading's line, which is the whole reason it exists", () => {
    // The layout rule lives in `ui.css` as `:has(> .ui-quiet)`, so what is
    // asserted here is the thing that rule needs: the quiet line is a DIRECT
    // child of the section. A wrapper div anywhere in between switches the page
    // silently back to two rows per empty section, and nothing else would notice.
    const { container } = render(
      <Section label="Commands">
        <Quiet says="none declared">Declaration rather than detection.</Quiet>
      </Section>,
    );

    const section = container.querySelector("section");
    expect(section?.getAttribute("aria-label")).toBe("Commands");
    expect(section?.querySelector(":scope > .ui-quiet")).toBeTruthy();
  });

  it("announces only when it is the answer to something you just did", () => {
    const { rerender } = render(<Quiet says="nothing came back" />);
    // A page opening with seven quiet panels must not announce seven absences.
    expect(screen.queryByRole("status")).toBeNull();

    rerender(<Quiet says="that provider is unavailable" announce />);
    expect(screen.getByRole("status").textContent).toContain("unavailable");
  });
});
