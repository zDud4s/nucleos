import { afterEach, describe, expect, it } from "vitest";
import { lastPlace, rememberPlace } from "./last-place";

afterEach(() => {
  localStorage.clear();
});

describe("where the window opens", () => {
  it("opens on Home when nothing has been remembered", () => {
    expect(lastPlace()).toBe("/");
  });

  it("reopens where it was left", () => {
    rememberPlace("/chats/c-1");
    expect(lastPlace()).toBe("/chats/c-1");
  });

  it("refuses a remembered value that is not a path of ours", () => {
    // Read from storage a person can edit and handed straight to the router. The rule is narrow on
    // purpose: a remembered place is a convenience, and there is no version of it worth a launch
    // that lands somewhere nobody asked for.
    for (const hostile of [
      "https://example.com/",
      "//example.com/",
      "/chats/../../etc",
      "chats/c-1",
      "",
    ]) {
      localStorage.setItem("nucleos.last-place", hostile);
      expect(lastPlace(), hostile).toBe("/");
    }
  });

  it("does not write a value it would refuse to read", () => {
    rememberPlace("https://example.com/");
    expect(localStorage.getItem("nucleos.last-place")).toBeNull();
  });
});
