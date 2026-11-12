import { describe, expect, it } from "vitest";
import { isMac, isWindows, projectPathPlaceholder } from "./platform";

const WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/120.0";
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
const LINUX = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15";

describe("platform", () => {
  it("names the Mac and Windows by their user agent", () => {
    expect(isMac(MAC)).toBe(true);
    expect(isMac(WINDOWS)).toBe(false);
    expect(isMac(LINUX)).toBe(false);
    expect(isWindows(WINDOWS)).toBe(true);
    expect(isWindows(MAC)).toBe(false);
    expect(isWindows(LINUX)).toBe(false);
  });

  it("writes the placeholder path the way this machine spells one", () => {
    expect(projectPathPlaceholder(WINDOWS)).toBe("C:\path\to\the\project");
    expect(projectPathPlaceholder(MAC)).toBe("/path/to/the/project");
    expect(projectPathPlaceholder(LINUX)).toBe("/path/to/the/project");
  });
});
