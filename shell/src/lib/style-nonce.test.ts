import { describe, expect, it } from "vitest";
import { getNonce } from "get-nonce";

import { adoptStyleNonce } from "./style-nonce";

/**
 * What can and cannot be tested here, stated so nobody mistakes green for covered.
 *
 * **Cannot:** whether the nonce actually satisfies the policy. jsdom does not enforce a
 * Content-Security-Policy, so a test here could not tell a working nonce from a decorative one.
 * That is `scripts/csp-gate.mjs`'s job, in a browser that enforces one, and it fails when this
 * mechanism stops working.
 *
 * **Can:** that the value is read the way a browser exposes it. Which is the half that actually
 * broke twice while this was being written, both times silently.
 */
function withDocument(head: string): Document {
  const doc = document.implementation.createHTMLDocument("");
  doc.head.innerHTML = head;
  return doc;
}

describe("adoptStyleNonce", () => {
  it("reads the nonce off the stamped tag and hands it to the injectors", () => {
    expect(adoptStyleNonce(withDocument('<style nonce="12345"></style>'))).toBe("12345");
    // The point of the whole module: `react-style-singleton` asks `get-nonce` for this, and puts
    // it on every stylesheet it builds.
    expect(getNonce()).toBe("12345");
  });

  /**
   * A `vite dev` document, and the reason this is `null` rather than a throw: there is no nonce in
   * that policy either, because `devCsp` allows the styles outright. Absent is a state, not a
   * fault.
   */
  it("says so plainly when nothing stamped a tag, rather than failing", () => {
    expect(adoptStyleNonce(withDocument("<style></style>"))).toBeNull();
    expect(adoptStyleNonce(withDocument("<title>nothing</title>"))).toBeNull();
  });

  /**
   * **Nonce hiding, and it is the whole reason this is not a one-liner.** A browser blanks the
   * `nonce` content attribute once the document has a nonce-carrying policy, so that a CSS
   * attribute selector cannot read the value back out — while the IDL property keeps it. Code that
   * reached for `getAttribute` alone would get an empty string in exactly the situation this
   * function exists for, and would report "no nonce" on a document that has one.
   */
  it("prefers the property over the attribute, because a browser blanks the attribute", () => {
    const doc = withDocument('<style nonce="visible"></style>');
    const style = doc.querySelector("style") as HTMLStyleElement;
    /*
      Staged rather than reproduced, and that is worth saying out loud: jsdom reflects `nonce`
      straight to the attribute, so the two cannot diverge there however they are set — the state
      a real browser presents is unreachable by ordinary means. Defining the property directly is
      the only way to put this branch under test at all, and the branch is the one that decides
      whether a document WITH a nonce reports having none.
    */
    Object.defineProperty(style, "nonce", { value: "the-real-one" });
    style.setAttribute("nonce", "");

    expect(adoptStyleNonce(doc)).toBe("the-real-one");
  });

  /** An empty attribute with no property behind it is nothing, and must not be handed on as one. */
  it("treats an empty nonce as no nonce", () => {
    expect(adoptStyleNonce(withDocument('<style nonce=""></style>'))).toBeNull();
  });
});
