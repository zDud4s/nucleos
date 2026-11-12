// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { dropUntrusted } from "./trust";

describe("dropUntrusted", () => {
  it("dropUntrusted stops a synthetic click before the panel", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = host.attachShadow({ mode: "open" });
    const button = document.createElement("button");
    root.appendChild(button);

    const reached = vi.fn();
    button.addEventListener("click", reached);
    dropUntrusted(root);

    // Script-made events are never trusted, whoever dispatches them.
    const synthetic = new MouseEvent("click", { bubbles: true, cancelable: true, composed: true });
    expect(synthetic.isTrusted).toBe(false);
    button.dispatchEvent(synthetic);
    button.click();

    expect(reached).not.toHaveBeenCalled();
    expect(synthetic.defaultPrevented).toBe(true);
  });

  it("dropUntrusted covers keydown, input, submit and pointerdown too", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = host.attachShadow({ mode: "open" });
    const field = document.createElement("input");
    root.appendChild(field);
    dropUntrusted(root);

    for (const type of ["keydown", "input", "submit", "pointerdown"]) {
      const reached = vi.fn();
      field.addEventListener(type, reached);
      field.dispatchEvent(new Event(type, { bubbles: true, cancelable: true, composed: true }));
      expect(reached, type).not.toHaveBeenCalled();
    }
  });
});
