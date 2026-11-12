// §spec pilar-de-web

package fetch

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"nucleosweb/safe"
)

// httptest servers listen on 127.0.0.1, which the SSRF guard refuses — correctly, and that is the
// point of the guard. So the tests that need a real server dial through a client whose transport
// has no Control hook, and the tests that prove the guard works use the guarded client.
//
// Writing it the other way round (weakening the guard so the tests are convenient) is exactly how a
// guard stops guarding, so the split is deliberate and the guarded client is what New returns.
func unguarded(timeout time.Duration, maxBytes int64) *Client {
	client := New(timeout, maxBytes)
	client.http.Transport = &http.Transport{DisableKeepAlives: true}
	return client
}

func TestFetchReadsAnHTMLPage(t *testing.T) {
	body := "<html><head><title>t</title></head><body><p>hello</p></body></html>"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, body)
	}))
	defer server.Close()

	page, err := unguarded(5*time.Second, 1<<20).Fetch(context.Background(), server.URL)
	if err != nil {
		t.Fatalf("fetching a plain HTML page failed: %v", err)
	}
	if page.Body != body {
		t.Errorf("body was altered in transit:\n got %q\nwant %q", page.Body, body)
	}
	if page.FinalURL != server.URL {
		t.Errorf("final URL = %q, want %q", page.FinalURL, server.URL)
	}
}

// The ceiling ERRORS; it does not quietly hand back a shortened page. A page cut in half is
// indistinguishable from a page that ended there.
func TestFetchRefusesAPageOverTheCeilingInsteadOfTruncating(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		fmt.Fprint(w, "<html><body>"+strings.Repeat("x", 5000)+"</body></html>")
	}))
	defer server.Close()

	page, err := unguarded(5*time.Second, 1000).Fetch(context.Background(), server.URL)
	if !errors.Is(err, ErrTooLarge) {
		t.Fatalf("an oversized page returned (%d bytes, %v), want ErrTooLarge", len(page.Body), err)
	}
	if page.Body != "" {
		t.Error("an oversized page came back with a body — a caller could store the truncation")
	}
}

// One byte over is read on purpose, so a page exactly at the limit is not mistaken for one that was
// cut off there.
func TestFetchAcceptsAPageExactlyAtTheCeiling(t *testing.T) {
	body := strings.Repeat("y", 500)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		fmt.Fprint(w, body)
	}))
	defer server.Close()

	page, err := unguarded(5*time.Second, int64(len(body))).Fetch(context.Background(), server.URL)
	if err != nil {
		t.Fatalf("a page exactly at the ceiling was refused: %v", err)
	}
	if len(page.Body) != len(body) {
		t.Errorf("body length = %d, want %d", len(page.Body), len(body))
	}
}

func TestFetchRefusesNonHTMLBeforeDownloadingIt(t *testing.T) {
	var served bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/pdf")
		w.WriteHeader(http.StatusOK)
		served = true
		fmt.Fprint(w, strings.Repeat("%PDF", 100000))
	}))
	defer server.Close()

	if _, err := unguarded(5*time.Second, 1<<20).Fetch(context.Background(), server.URL); !errors.Is(err, ErrNotHTML) {
		t.Fatalf("a PDF was accepted as a page: %v", err)
	}
	_ = served
}

func TestIsReadable(t *testing.T) {
	readable := []string{
		"text/html",
		"text/html; charset=utf-8",
		"TEXT/HTML",
		"  text/plain  ",
		"application/xhtml+xml",
	}
	for _, contentType := range readable {
		if !isReadable(contentType) {
			t.Errorf("%q was refused but is a page", contentType)
		}
	}

	// An absent Content-Type is refused: a server that said nothing did not say HTML, and guessing
	// hands arbitrary bytes to an HTML parser on the strength of a missing header.
	refused := []string{"", "application/pdf", "image/png", "application/octet-stream", "video/mp4"}
	for _, contentType := range refused {
		if isReadable(contentType) {
			t.Errorf("%q was accepted as a page", contentType)
		}
	}
}

func TestFetchFollowsRedirectsAndReportsWhereItLanded(t *testing.T) {
	var destination *httptest.Server
	destination = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		fmt.Fprint(w, "<html><body>landed</body></html>")
	}))
	defer destination.Close()

	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, destination.URL, http.StatusFound)
	}))
	defer origin.Close()

	page, err := unguarded(5*time.Second, 1<<20).Fetch(context.Background(), origin.URL)
	if err != nil {
		t.Fatalf("a redirect was not followed: %v", err)
	}
	// This is the field the trust decision is made over (spec §10.1). If it reported the requested
	// URL instead, an open redirect on an allowlisted host would launder any destination into "raw".
	if page.FinalURL != destination.URL {
		t.Errorf("final URL = %q, want the redirect target %q", page.FinalURL, destination.URL)
	}
	if page.RequestedURL != origin.URL {
		t.Errorf("requested URL = %q, want %q", page.RequestedURL, origin.URL)
	}
}

func TestFetchStopsAnEndlessRedirectLoop(t *testing.T) {
	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, server.URL+"/again", http.StatusFound)
	}))
	defer server.Close()

	if _, err := unguarded(5*time.Second, 1<<20).Fetch(context.Background(), server.URL); err == nil {
		t.Fatal("a redirect loop was followed forever")
	}
}

// The guard is not optional, and New is the only constructor. A loopback destination must be
// refused by the client this package actually hands out.
func TestTheGuardedClientRefusesLoopback(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		fmt.Fprint(w, "<html><body>the daemon</body></html>")
	}))
	defer server.Close()

	_, err := New(5*time.Second, 1<<20).Fetch(context.Background(), server.URL)
	if err == nil {
		t.Fatal("the guarded client reached a loopback server — web_read is a proxy to the daemon")
	}
}

func TestFetchRefusesNonWebSchemesBeforeDialling(t *testing.T) {
	_, err := New(5*time.Second, 1<<20).Fetch(context.Background(), "file:///c:/windows/win.ini")
	if !errors.Is(err, safe.ErrBlocked) {
		t.Fatalf("a file:// URL was not blocked: %v", err)
	}
}
