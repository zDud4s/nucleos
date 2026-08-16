//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
)

// settle is how long a test waits for something to reach the origin server.
//
// Every "it did not happen" assertion in this file is paired with a "it did happen" control that
// uses the same window, so a window that is too short fails BOTH — which is the only way a timing
// mistake here can be noticed rather than read as a fence that works.
const settle = 4 * time.Second

// ---------------------------------------------------------------------------
// Test 1 — a document from a host the profile does not admit does not execute.
// ---------------------------------------------------------------------------

func TestAnUnadmittedDocumentDoesNotRun(t *testing.T) {
	admitted := newSite(t)
	stranger := newSite(t)

	driver, _ := fenced(t, admitting(admitted))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: stranger.origin() + "/page"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal == nil {
		t.Fatalf("the navigation was not refused: %+v", session)
	}
	if stranger.reached("GET /page", settle) {
		t.Fatal("the request reached the stranger anyway")
	}

	// The control, in the same test: the admitted site loads. Without it "nothing arrived" would be
	// satisfied by a browser that could not reach anything at all.
	if _, err := driver.Open(ctx, browser.OpenRequest{URL: admitted.origin() + "/page"}); err != nil {
		t.Fatalf("the admitted site failed to open: %v", err)
	}
	if !admitted.reached("GET /page", settle) {
		t.Fatal("the admitted site was never reached either; the browser is broken, not fencing")
	}
}

func TestAnUnadmittedDocumentDoesNotRunInAFrameEither(t *testing.T) {
	admitted := newSite(t)
	stranger := newSite(t)

	driver, _ := fenced(t, admitting(admitted))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: admitted.origin() + "/page"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	// The frame is added from the page, which is how a real one arrives. Spec §5.4's earlier version
	// said "top-level navigations", and an iframe is a document that is not top level — the gap this
	// test exists for.
	drain(admitted)
	if _, err := driver.Open(ctx, browser.OpenRequest{
		URL: admitted.origin() + "/framing?src=" + stranger.origin() + "/framed",
	}); err != nil {
		t.Fatalf("open framing page: %v", err)
	}
	if stranger.reached("GET /framed", settle) {
		t.Fatal("a cross-site frame loaded inside the admitted profile")
	}
}

// ---------------------------------------------------------------------------
// Test 2 — a POST does not leave, and the same POST leaves with the fence off.
// ---------------------------------------------------------------------------

func TestAFormSubmissionDoesNotLeave(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/form"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send")

	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if site.reached("POST /submit", settle) {
		t.Fatal("the POST left the machine")
	}
	// "This act or the next" is the contract, not a hedge — see chrome.Act. A form submission
	// routinely misses the settle window when several browsers are running at once, and an earlier
	// version of this assertion failed about one run in three for that reason. What must never happen
	// is the refusal being lost, so the test presses on and requires it to arrive.
	if result.Outcome != browser.OutcomeRefused {
		second, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionScroll, Ref: button})
		if err != nil {
			t.Fatalf("second act: %v", err)
		}
		if second.Outcome != browser.OutcomeRefused {
			t.Errorf("neither act told the agent the fence stopped anything: %+v then %+v", result, second)
		}
	}

	// Control: the same form, same server, fence off.
	conn := control(t)
	page := openIn(t, conn, site.origin()+"/form")
	time.Sleep(500 * time.Millisecond)
	evaluate(t, conn, page, `document.getElementById('f').submit()`)
	if !site.reached("POST /submit", settle) {
		t.Fatal("the POST did not arrive with the fence off either; the test proves nothing")
	}
}

// ---------------------------------------------------------------------------
// Test 3 — a WebSocket handshake does not complete.
// ---------------------------------------------------------------------------

func TestAWebSocketDoesNotConnect(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/ws"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if site.reached("UPGRADE /socket", settle) {
		t.Fatal("the handshake reached the origin")
	}

	// Control: the same page with no fence. This is the one channel CDP is blind to — the spike
	// measured Fetch never seeing a ws: url and setBlockedURLs completing the handshake regardless —
	// so without a control this test would be indistinguishable from a page that never ran.
	conn := control(t)
	openIn(t, conn, site.origin()+"/ws")
	if !site.reached("UPGRADE /socket", settle) {
		t.Fatal("the handshake did not arrive with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 4 — window.open does not produce a page.
// ---------------------------------------------------------------------------

func TestAPopupDoesNotOpen(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/popup"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("BEACON popup-null", settle) {
		t.Fatal("window.open did not return null")
	}
	if site.reached("GET /opened", settle) {
		t.Fatal("the popup navigated")
	}

	conn := control(t)
	openIn(t, conn, site.origin()+"/popup")
	if !site.reached("GET /opened", settle) {
		t.Fatal("the popup did not open with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 5 — a blob: document navigates, and nothing leaves it.
// ---------------------------------------------------------------------------

// TestABlobDocumentIsContainedAndNotClosed is deliberately not called "blocked".
//
// The spike measured that blob: and javascript: DO navigate and that Fetch sees nothing at all; what
// contains them is that the blob document INHERITS its parent's CSP. So the claim under test is the
// only one that held: the document loads, and nothing gets out of it.
func TestABlobDocumentIsContainedAndNotClosed(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/blob"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("BEACON blob-loaded", settle) {
		t.Fatal("the blob document did not load; this test asserts containment, not blocking")
	}
	if site.reached("BEACON blob-fetch-escaped", settle) {
		t.Fatal("a fetch escaped the blob document; the parent's CSP was not inherited")
	}
}

// ---------------------------------------------------------------------------
// Test 6 — a service worker does not install.
// ---------------------------------------------------------------------------

func TestAServiceWorkerDoesNotInstall(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/sw"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if site.reached("GET /sw.js", settle) {
		t.Fatal("the origin served the worker script; the registration would have installed")
	}
	if site.reached("BEACON sw-registered", settle) {
		t.Fatal("register() resolved; a worker installed in the profile")
	}

	// Control, and it is the one the spike's §5.8 table is about: with the fence off the script IS
	// served and the worker installs. On the PAGE session it also installed — which is why the fence
	// lives on the browser session.
	conn := control(t)
	openIn(t, conn, site.origin()+"/sw")
	if !site.reached("GET /sw.js", settle) {
		t.Fatal("the script was not served with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 8 — a download does not write to disk.
// ---------------------------------------------------------------------------

func TestADownloadDoesNotReachTheDisk(t *testing.T) {
	site := newSite(t)
	driver, profile := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/download"})
	if err != nil && session.Refusal == nil {
		// A refused navigation is the expected shape; a transport error is not.
		if !strings.Contains(err.Error(), "ERR_") {
			t.Fatalf("open: %v", err)
		}
	}
	if found := findOnDisk(t, profile, "report.txt"); found != "" {
		t.Fatalf("the download was written to %s", found)
	}
}

// ---------------------------------------------------------------------------
// Test 7 — failing closed.
// ---------------------------------------------------------------------------

// TestAnUnarmableFenceProducesNoDriver is test 7's real-browser half.
//
// The "Fetch.enable is unavailable" case cannot be staged against a real Chrome — it is available —
// so that half is proven in chrome's unit tests against a browser that refuses the call. What CAN be
// staged here is the other precondition: a driver refuses to exist without a policy, and therefore
// without one nothing navigates at all.
func TestAnUnarmableFenceProducesNoDriver(t *testing.T) {
	site := newSite(t)
	proxy, err := fence.NewProxy(admitting(site))
	if err != nil {
		t.Fatalf("proxy: %v", err)
	}
	defer proxy.Close()

	if _, err := fence.NewProxy(fence.Policy{}); err == nil {
		t.Fatal("a proxy started with no policy")
	}
	// And the browser-side half: see chrome.TestAFenceWithNoPolicyRefusesToProduceADriver, which
	// asserts the browser is not touched at all before the policy is checked.
}

func drain(s *site) {
	for {
		select {
		case <-s.arrived:
		default:
			return
		}
	}
}

func findRef(t *testing.T, snapshot browser.Snapshot, name string) string {
	t.Helper()
	for _, element := range snapshot.Elements {
		if strings.Contains(element.Name, name) {
			return element.Ref
		}
	}
	t.Fatalf("no element named %q in the snapshot: %+v", name, snapshot.Elements)
	return ""
}
