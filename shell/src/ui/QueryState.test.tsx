import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryState, type QueryLike } from "./QueryState";

function q<T>(over: Partial<QueryLike<T>>): QueryLike<T> {
  return {
    data: undefined,
    error: null,
    isPending: false,
    isError: false,
    dataUpdatedAt: 0,
    refetch: () => {},
    ...over,
  };
}

describe("QueryState", () => {
  it("shows a busy skeleton while pending, never the empty text", () => {
    render(
      <QueryState query={q<string[]>({ isPending: true })} label="Loading teams" empty="No team yet">
        {(d) => <p>{d.length}</p>}
      </QueryState>,
    );
    expect(screen.getByRole("status").getAttribute("aria-busy")).toBe("true");
    expect(screen.queryByText("No team yet")).toBeNull();
  });

  it("shows the error with its cause and a Retry that refetches", () => {
    const refetch = vi.fn();
    render(
      <QueryState
        query={q<string[]>({ isError: true, error: new Error("daemon stopped?"), refetch })}
        label="teams"
      >
        {() => <p>data</p>}
      </QueryState>,
    );
    expect(screen.getByRole("alert").textContent).toContain("daemon stopped?");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(refetch).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("data")).toBeNull();
  });

  it("keeps the last good data and says it is stale when a refetch failed", () => {
    render(
      <QueryState
        query={q<string[]>({ isError: true, error: new Error("x"), data: ["a"], dataUpdatedAt: Date.now() })}
        label="teams"
      >
        {(d) => <p>rows {d.length}</p>}
      </QueryState>,
    );
    expect(screen.getByText("rows 1")).toBeDefined();
    expect(screen.getByText(/view is stale/)).toBeDefined();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows the empty text only once the data has arrived and is empty", () => {
    render(
      <QueryState query={q<string[]>({ data: [] })} label="teams" empty="No team yet">
        {() => <p>data</p>}
      </QueryState>,
    );
    expect(screen.getByText("No team yet")).toBeDefined();
  });

  it("honours a custom isEmpty for non-array data", () => {
    render(
      <QueryState query={q<{ n: number }>({ data: { n: 0 } })} label="x" empty="nothing" isEmpty={(d) => d.n === 0}>
        {() => <p>data</p>}
      </QueryState>,
    );
    expect(screen.getByText("nothing")).toBeDefined();
  });

  it("renders unknown as a dash, not as zero or idle, when there is no data and no failure", () => {
    render(
      <QueryState query={q<number>({})} label="runs">
        {(d) => <p>{d}</p>}
      </QueryState>,
    );
    const el = screen.getByText("—");
    expect(el.getAttribute("aria-label") ?? el.textContent).toBeTruthy();
    expect(screen.queryByText("0")).toBeNull();
  });

  it("renders data through children", () => {
    render(
      <QueryState query={q<string[]>({ data: ["a", "b"] })} label="teams">
        {(d) => <p>rows {d.length}</p>}
      </QueryState>,
    );
    expect(screen.getByText("rows 2")).toBeDefined();
  });
});
