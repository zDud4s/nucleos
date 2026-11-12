import { describe, expect, it } from "vitest";
import { resolveOmnibox, SEARCH_ENGINE_URL } from "./browser-omnibox";

const search = (q: string) => SEARCH_ENGINE_URL + encodeURIComponent(q);

describe("resolveOmnibox", () => {
  it("returns null for empty input", () => {
    expect(resolveOmnibox("")).toBeNull();
    expect(resolveOmnibox("   ")).toBeNull();
  });

  it("keeps explicit http(s) and about: as is", () => {
    expect(resolveOmnibox(" https://a.com/x ")).toBe("https://a.com/x");
    expect(resolveOmnibox("http://a.com")).toBe("http://a.com");
    expect(resolveOmnibox("about:blank")).toBe("about:blank");
  });

  it("prefixes hosts with a dot with https", () => {
    expect(resolveOmnibox("example.com/path")).toBe("https://example.com/path");
  });

  it("prefixes localhost and IPs with http", () => {
    expect(resolveOmnibox("localhost")).toBe("http://localhost");
    expect(resolveOmnibox("localhost:3000/app")).toBe("http://localhost:3000/app");
    expect(resolveOmnibox("192.168.1.1:8080")).toBe("http://192.168.1.1:8080");
  });

  it("searches everything else", () => {
    expect(resolveOmnibox("hello world")).toBe(search("hello world"));
    expect(resolveOmnibox("example.com is down")).toBe(search("example.com is down"));
    expect(resolveOmnibox("cats")).toBe(search("cats"));
  });

  it("never navigates to dangerous schemes", () => {
    for (const s of ["javascript:alert(1)", "file:///etc/passwd", "data:text/html,hi"]) {
      expect(resolveOmnibox(s)).toBe(search(s));
    }
  });
});
