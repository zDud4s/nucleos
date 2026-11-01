import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { RefusalNote } from "./RefusalNote";
import { ApiRefusal } from "../data/client";
import { ErrorNote } from "./ErrorNote";

// `client.ts` reaches for the Tauri bridge to read the daemon token. Nothing in
// this file calls it — `ApiRefusal` is a plain class — but importing the module
// under jsdom still pulls the bridge in.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("RefusalNote", () => {
  it("turns a known code into its named sentence", () => {
    render(<RefusalNote refusal={new ApiRefusal(423, "kill_switch", "kill_switch")} />);

    const note = screen.getByRole("status").textContent ?? "";
    expect(note).toMatch(/kill switch is engaged/i);
    // Never the generic sentence that tells a person nothing about what to do.
    expect(note).not.toMatch(/request failed/i);
    // The stable name travels with the sentence: it is what survives rewording,
    // and it is what somebody quotes when the sentence is not enough.
    expect(note).toContain("kill_switch");
  });

  it("does not repeat the code as its own detail", () => {
    // The núcleo answers `{"refusal":"no_local_model"}` and the client uses the
    // name for both fields; echoing it twice reads as a stutter.
    render(<RefusalNote refusal={new ApiRefusal(503, "no_local_model", "no_local_model")} />);
    const note = screen.getByRole("status").textContent ?? "";
    expect(note).toMatch(/no local model is available/i);
    expect(note.match(/no_local_model/g)?.length).toBe(1);
  });

  it("lets a page sharpen the sentence for its own route", () => {
    // A 409 on `POST /jobs` is "that project is full"; a 409 on a merge is
    // "somebody already decided the opposite way". Only the route knows.
    render(
      <RefusalNote
        refusal={new ApiRefusal(409, "conflict", "")}
        sentences={{ conflict: "that project is already at its ceiling" }}
      />,
    );
    expect(screen.getByRole("status").textContent).toContain("that project is already at its ceiling");
  });

  it("falls back to the daemon's prose before inventing anything", () => {
    // A code this table has never heard of, with real prose behind it: the
    // prose wins over a sentence built from the status, because the route wrote
    // it and the route knows.
    render(<RefusalNote refusal={new ApiRefusal(418, "http_418", "the pot is not a teapot")} />);

    const note = screen.getByRole("status").textContent ?? "";
    expect(note).toContain("the pot is not a teapot");
    expect(note).not.toMatch(/request failed/i);
  });

  it("still names the refusal when there is neither copy nor prose", () => {
    render(<RefusalNote refusal={new ApiRefusal(418, "http_418", "")} />);
    // The floor: the code, in a sentence. Never nothing, and never "an error
    // occurred" — a name is something a person can search for.
    expect(screen.getByRole("status").textContent).toContain("http_418");
  });

  it("is not the ErrorNote shape — a refusal is a value, not a failure", () => {
    render(<RefusalNote refusal={new ApiRefusal(423, "kill_switch", "kill_switch")} />);

    // `alert` is assertive and interrupts. Nothing broke here: the daemon
    // answered, and it answered no.
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status")).toBeDefined();
    expect(screen.getByRole("status").className).toContain("ui-note-refusal");
    expect(screen.getByRole("status").className).not.toContain("ui-note-error");
  });

  it("reads differently from a transport error side by side", () => {
    const { container } = render(
      <div>
        <RefusalNote refusal={new ApiRefusal(423, "kill_switch", "kill_switch")} />
        <ErrorNote>the service is unreachable</ErrorNote>
      </div>,
    );

    const notes = Array.from(container.querySelectorAll(".ui-note"));
    expect(notes).toHaveLength(2);
    // Two roles, two classes: the distinction is carried semantically and
    // visually, not by wording alone.
    expect(notes.map((n) => n.getAttribute("role"))).toEqual(["status", "alert"]);
    expect(notes[0].className).not.toBe(notes[1].className);
  });

  it("no_gemini_key and no_openrouter_key read as sentences; warning_not_acknowledged shows the daemon's detail", () => {
    const gemini = render(<RefusalNote refusal={new ApiRefusal(503, "no_gemini_key", "no_gemini_key")} />);
    expect(screen.getByRole("status").textContent ?? "").toMatch(/gemini api key/i);
    expect(screen.getByRole("status").textContent ?? "").not.toMatch(/refused this/i);
    gemini.unmount();

    const router = render(
      <RefusalNote refusal={new ApiRefusal(503, "no_openrouter_key", "no_openrouter_key")} />,
    );
    expect(screen.getByRole("status").textContent ?? "").toMatch(/openrouter/i);
    expect(screen.getByRole("status").textContent ?? "").not.toMatch(/refused this/i);
    router.unmount();

    // No page sentence for this code: the daemon's warning text must reach the screen.
    const warning = "agy runs unattended with your Google account; read this before enabling it";
    render(<RefusalNote refusal={new ApiRefusal(400, "warning_not_acknowledged", warning)} />);
    expect(screen.getByRole("status").textContent).toContain(warning);
  });
});
