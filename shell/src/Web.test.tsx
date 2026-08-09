import { afterEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Web from "./Web";
import type { WebHit, WebPage } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function hit(overrides: Partial<WebHit> = {}): WebHit {
  return {
    id: 1,
    final_url: "https://developer.mozilla.org/en-US/docs/Web/API/fetch",
    host: "developer.mozilla.org",
    title: "fetch()",
    snippet: "The fetch() method starts the process of fetching a resource.",
    trust_at_fetch: "raw",
    fetched_at: "2026-08-02T10:00:00Z",
    ...overrides,
  };
}

function page(overrides: Partial<WebPage> = {}): WebPage {
  return {
    id: 1,
    requested_url: "https://developer.mozilla.org/en-US/docs/Web/API/fetch",
    final_url: "https://developer.mozilla.org/en-US/docs/Web/API/fetch",
    host: "developer.mozilla.org",
    title: "fetch()",
    byline: null,
    content_md: "The fetch() method starts the process of fetching a resource.",
    extract_status: "article",
    trust_at_fetch: "raw",
    trust_rule: "owner-allowlisted",
    bytes: 4096,
    fetched_at: "2026-08-02T10:00:00Z",
    ...overrides,
  };
}

function daemonHolding(hits: WebHit[], full: WebPage = page()) {
  fetchMock.mockImplementation((url: string) => {
    if (/\/web\/pages\/\d+$/.test(url)) {
      return Promise.resolve({ ok: true, json: async () => full });
    }
    return Promise.resolve({ ok: true, json: async () => hits });
  });
}

async function show(hits: WebHit[], full?: WebPage) {
  daemonHolding(hits, full);
  await act(async () => {
    render(<Web token="t" connection="connected" />);
  });
}

afterEach(() => {
  fetchMock.mockReset();
});

describe("the web archive", () => {
  it("lists what has been read", async () => {
    await show([hit()]);
    expect(screen.getByText("fetch()")).toBeTruthy();
  });

  /**
   * The badge is the reason this tab exists. It is the only place a person can see that a
   * stranger's prose entered an agent's context, and in which form — so a page read as written and
   * a page a local model summarised must never look the same.
   */
  it("says whether the agent saw the page or a summary of it", async () => {
    await show([
      hit({ id: 1, trust_at_fetch: "raw", title: "trusted" }),
      hit({
        id: 2,
        trust_at_fetch: "quarantined",
        title: "unknown source",
        final_url: "https://somewhere.example/post",
      }),
    ]);

    expect(screen.getByText("read as written")).toBeTruthy();
    expect(screen.getByText("summarised locally")).toBeTruthy();
  });

  /** A redirect can only ever lose trust, and a reader deserves to see that one happened. */
  it("shows when a page came from somewhere other than the URL that was asked for", async () => {
    await show(
      [hit()],
      page({
        requested_url: "https://short.example/x",
        final_url: "https://somewhere.example/landed",
        trust_at_fetch: "quarantined",
        trust_rule: "redirected-out-of-allowlist",
      }),
    );

    await act(async () => {
      fireEvent.click(screen.getByText("fetch()"));
    });

    expect(screen.getByText(/Redirected here from/)).toBeTruthy();
    expect(screen.getByText(/short\.example/)).toBeTruthy();
  });

  /**
   * A fallback extraction is a page's structure and not its prose. Saying so matters because the
   * alternative — presenting an accessibility tree as though it were the article — is how a reader
   * concludes a page said nothing when it was simply not readable without a browser.
   */
  it("says when what was extracted is structure rather than prose", async () => {
    await show([hit()], page({ extract_status: "fallback" }));

    await act(async () => {
      fireEvent.click(screen.getByText("fetch()"));
    });

    expect(screen.getByText(/structure rather than its prose/)).toBeTruthy();
  });

  it("says the pillar is off rather than showing an empty list with no explanation", async () => {
    await show([]);
    expect(screen.getByText(/enabled: true in \.ai\/web\.yaml/)).toBeTruthy();
  });

  it("reads nothing when there is no token", async () => {
    await act(async () => {
      render(<Web token={null} connection="connected" />);
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  /**
   * The archive search stays exactly one box, and it is still the local one.
   *
   * The tab now has a second field that fetches, so the risk this pins is the opposite of the one it
   * used to: not that a URL control exists, but that the two get confused. Searching what has been
   * read must never become a call that spends an API request on a provider.
   */
  it("keeps the archive search local and separate from the address field", async () => {
    await show([hit()]);

    const searches = screen.getAllByRole("searchbox");
    expect(searches).toHaveLength(1);
    expect(searches[0].getAttribute("aria-label")).toBe("Search what has been read");

    fetchMock.mockClear();
    fireEvent.change(searches[0], { target: { value: "fetch" } });
    await act(async () => {
      fireEvent.submit(searches[0].closest("form")!);
    });

    const called = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(called.every((url) => url.includes("/web/pages"))).toBe(true);
    expect(called.some((url) => url.includes("/web/search"))).toBe(false);
  });

  it("fetches a page through the daemon's own read route", async () => {
    await show([hit()]);

    fetchMock.mockClear();
    fetchMock.mockImplementation((url: string) => {
      if (String(url).includes("/web/read")) {
        return Promise.resolve({ ok: true, json: async () => ({ id: 1, from_cache: false }) });
      }
      if (/\/web\/pages\/\d+$/.test(String(url))) {
        return Promise.resolve({ ok: true, json: async () => page() });
      }
      return Promise.resolve({ ok: true, json: async () => [hit()] });
    });

    const address = screen.getByLabelText("Address to read");
    fireEvent.change(address, { target: { value: "https://example.com/a" } });
    await act(async () => {
      fireEvent.submit(address.closest("form")!);
    });

    const read = fetchMock.mock.calls.find((call) => String(call[0]).includes("/web/read"));
    expect(read).toBeTruthy();
    expect(read![1].method).toBe("POST");
    expect(JSON.parse(read![1].body)).toEqual({ url: "https://example.com/a" });
    // What was filed is what gets shown, read back from the archive rather than rendered from the
    // fetch's own reply. `bytes` is the proof: the read's own response has no such field, so a
    // reader rendered from it could not show this number at all.
    expect(fetchMock.mock.calls.some((call) => /\/web\/pages\/1$/.test(String(call[0])))).toBe(true);
    // Locale-independent on purpose: the number is run through `toLocaleString`, and pinning its
    // separator would make this test a statement about the machine it runs on.
    expect(screen.getByText(/bytes of source/)).toBeTruthy();
  });

  /** A refused pillar is a switch to flip, not an outage, and must not be reported as one. */
  it("says the pillar is off when a read is refused with 503", async () => {
    await show([hit()]);

    fetchMock.mockImplementation(() =>
      Promise.resolve({ ok: false, status: 503, text: async () => "the web pillar is disabled" }),
    );

    const address = screen.getByLabelText("Address to read");
    fireEvent.change(address, { target: { value: "https://example.com/a" } });
    await act(async () => {
      fireEvent.submit(address.closest("form")!);
    });

    expect(screen.getAllByText(/enabled: true in \.ai\/web\.yaml/).length).toBeGreaterThan(0);
  });
});
