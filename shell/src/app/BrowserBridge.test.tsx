import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  navigate: vi.fn(),
  pathname: "/",
  stop: vi.fn(),
  start: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mocks.navigate,
  useRouterState: ({ select }: { select: (s: unknown) => unknown }) =>
    select({ location: { pathname: mocks.pathname } }),
}));

vi.mock("../data/browser-store", async (original) => ({
  ...(await original<typeof import("../data/browser-store")>()),
  startBrowserBridge: mocks.start,
}));

import { requestFocus, resetBrowserStore } from "../data/browser-store";
import { BrowserBridge } from "./BrowserBridge";

beforeEach(() => {
  mocks.navigate.mockReset();
  mocks.stop.mockReset();
  mocks.start.mockReset();
  mocks.start.mockReturnValue(mocks.stop);
  mocks.pathname = "/";
  resetBrowserStore();
});

afterEach(() => resetBrowserStore());

describe("BrowserBridge", () => {
  it("starts the bridge on mount and stops it on unmount", () => {
    const { unmount } = render(<BrowserBridge />);
    expect(mocks.start).toHaveBeenCalledTimes(1);
    expect(mocks.stop).not.toHaveBeenCalled();
    unmount();
    expect(mocks.stop).toHaveBeenCalledTimes(1);
  });

  it("navigates to /web on a focus intent from another route", () => {
    mocks.pathname = "/runs";
    render(<BrowserBridge />);
    expect(mocks.navigate).not.toHaveBeenCalled();
    act(() => requestFocus("p", "s1"));
    expect(mocks.navigate).toHaveBeenCalledWith({ to: "/web" });
  });

  it("does not navigate when already on /web or below it", () => {
    mocks.pathname = "/web";
    const first = render(<BrowserBridge />);
    act(() => requestFocus("p", "s1"));
    first.unmount();
    mocks.pathname = "/web/archive";
    render(<BrowserBridge />);
    expect(mocks.navigate).not.toHaveBeenCalled();
  });

  it("navigates once per nonce", () => {
    mocks.pathname = "/runs";
    const { rerender } = render(<BrowserBridge />);
    act(() => requestFocus("p", "s1"));
    rerender(<BrowserBridge />);
    rerender(<BrowserBridge />);
    expect(mocks.navigate).toHaveBeenCalledTimes(1);
    act(() => requestFocus("p", "s2"));
    expect(mocks.navigate).toHaveBeenCalledTimes(2);
  });
});
