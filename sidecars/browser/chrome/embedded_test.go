// §spec pilar-de-browser

package chrome

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
	"nucleosbrowser/fence"
)

// attachable makes the fake answer Target.attachToTarget the way Chrome does for a flattened attach.
func attachable(fake *cdptest.Browser) {
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"sessionId": "SE1"}, nil
	})
	fake.Handle("Target.getTargetInfo", func(cdptest.Call) (any, error) {
		return map[string]any{"targetInfo": map[string]any{
			"targetId": "TE1",
			"url":      "https://example.org/landed",
			"title":    "Example",
		}}, nil
	})
}

func connectEmbedded(t *testing.T) (*cdptest.Browser, *cdp.Conn, *Driver) {
	t.Helper()
	fake, conn := dial(t)
	attachable(fake)
	driver, err := ConnectEmbedded(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect embedded: %v", err)
	}
	return fake, conn, driver
}

func TestConnectEmbeddedArmsTheBrowserSessionWithoutAutoAttach(t *testing.T) {
	fake, _, driver := connectEmbedded(t)
	if !driver.embedded {
		t.Fatal("driver is not marked embedded")
	}
	calls := fake.Calls()
	if fake.IndexOf("Fetch.enable") < 0 || fake.IndexOf("Browser.setDownloadBehavior") < 0 {
		t.Fatalf("fence not armed: %v", fake.Methods())
	}
	for _, call := range calls {
		if call.Method == "Fetch.enable" && call.Session != "" {
			t.Errorf("Fetch.enable went on session %q, want the browser session", call.Session)
		}
	}
	for _, forbidden := range []string{"Target.setAutoAttach", "Target.createTarget"} {
		if slices.Contains(fake.Methods(), forbidden) {
			t.Errorf("%s was sent: %v", forbidden, fake.Methods())
		}
	}
}

func TestEmbeddedConnectionRefusesCreateTargetAndStorage(t *testing.T) {
	fake, conn, _ := connectEmbedded(t)
	for _, method := range []string{"Target.createTarget", "Storage.clearDataForOrigin", "Storage.getCookies"} {
		_, err := conn.Call(context.Background(), cdp.BrowserSession, method, nil)
		if !errors.Is(err, cdp.ErrMethodRefused) {
			t.Errorf("%s: err = %v, want ErrMethodRefused", method, err)
		}
		if slices.Contains(fake.Methods(), method) {
			t.Errorf("%s reached the browser", method)
		}
	}
}

func TestConnectEmbeddedRefusesAnInvalidPolicy(t *testing.T) {
	fake, conn := dial(t)
	_, err := ConnectEmbedded(context.Background(), conn, fence.Policy{})
	if !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("err = %v, want ErrFenceNotAttached", err)
	}
	if len(fake.Calls()) != 0 {
		t.Errorf("calls went out before the policy was checked: %v", fake.Methods())
	}
}

func TestAdoptFencesTheAttachedTabBeforeNavigating(t *testing.T) {
	fake, _, driver := connectEmbedded(t)

	session, err := driver.Adopt(context.Background(), "TE1", "https://example.org/")
	if err != nil {
		t.Fatalf("adopt: %v", err)
	}
	if session.ID == "" || session.RequestedURL != "https://example.org/" {
		t.Fatalf("session = %+v", session)
	}

	navigateAt := fake.IndexOf("Page.navigate")
	attachAt := fake.IndexOf("Target.attachToTarget")
	if navigateAt < 0 || attachAt < 0 {
		t.Fatalf("missing calls: %v", fake.Methods())
	}
	if attachAt > navigateAt {
		t.Errorf("attached after navigating: %v", fake.Methods())
	}
	for _, call := range fake.Calls() {
		if call.Method == "Target.attachToTarget" &&
			(!containsJSON(call.Params, `"targetId":"TE1"`) || !containsJSON(call.Params, `"flatten":true`)) {
			t.Errorf("attach params = %s", call.Params)
		}
	}
	for _, earlier := range []string{"Page.enable", "Log.enable", "Target.setAutoAttach"} {
		at := fake.IndexOf(earlier)
		if at < 0 {
			t.Errorf("%s never happened: %v", earlier, fake.Methods())
		} else if at < attachAt || at > navigateAt {
			t.Errorf("%s not between the attach and the navigation: %v", earlier, fake.Methods())
		}
	}
	for _, call := range fake.Calls() {
		if call.Method == "Target.setAutoAttach" && call.Session != "SE1" {
			t.Errorf("page auto-attach on session %q, want SE1", call.Session)
		}
		if call.Method == "Page.navigate" && call.Session != "SE1" {
			t.Errorf("navigated on session %q, want SE1", call.Session)
		}
	}
	if slices.Contains(fake.Methods(), "Target.createTarget") {
		t.Errorf("created a target: %v", fake.Methods())
	}
	driver.mu.Lock()
	registered := driver.sessions[session.ID] != nil && driver.targets["TE1"] == session.ID
	driver.mu.Unlock()
	if !registered {
		t.Error("session not registered")
	}
}

func TestOpenOnAnEmbeddedDriverIsRefused(t *testing.T) {
	fake, _, driver := connectEmbedded(t)
	_, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if !errors.Is(err, ErrEmbeddedOpen) {
		t.Fatalf("err = %v, want ErrEmbeddedOpen", err)
	}
	if slices.Contains(fake.Methods(), "Target.createTarget") {
		t.Errorf("createTarget sent: %v", fake.Methods())
	}
}

func TestAdoptAttachErrorRegistersNothing(t *testing.T) {
	fake, _, driver := connectEmbedded(t)
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return nil, errors.New("no such target")
	})
	if _, err := driver.Adopt(context.Background(), "TE1", "https://example.org/"); err == nil {
		t.Fatal("adopt succeeded")
	}
	driver.mu.Lock()
	n := len(driver.sessions) + len(driver.targets) + len(driver.cdpToSession)
	driver.mu.Unlock()
	if n != 0 {
		t.Errorf("%d entries registered", n)
	}
	if slices.Contains(fake.Methods(), "Page.navigate") {
		t.Error("navigated after a failed attach")
	}
}

func TestAdoptFailureBeforeRegistrationDetachesAndNeverClosesTheTab(t *testing.T) {
	fake, _, driver := connectEmbedded(t)
	fake.Handle("Page.enable", func(cdptest.Call) (any, error) {
		return nil, errors.New("boom")
	})
	if _, err := driver.Adopt(context.Background(), "TE1", "https://example.org/"); err == nil {
		t.Fatal("adopt succeeded")
	}
	if fake.IndexOf("Target.detachFromTarget") < 0 {
		t.Errorf("no detach: %v", fake.Methods())
	}
	if slices.Contains(fake.Methods(), "Target.closeTarget") {
		t.Errorf("closed the shell's tab: %v", fake.Methods())
	}
}

func TestAdoptOnAHeadlessDriverIsRefused(t *testing.T) {
	fake, conn := dial(t)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Adopt(context.Background(), "TE1", "https://example.org/"); err == nil {
		t.Fatal("adopt succeeded on a headless driver")
	}
	if slices.Contains(fake.Methods(), "Target.attachToTarget") {
		t.Error("attached from a headless driver")
	}
}

func containsJSON(raw []byte, fragment string) bool {
	return strings.Contains(string(raw), fragment)
}

func adoptedEmbedded(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, _, driver := connectEmbedded(t)
	session, err := driver.Adopt(context.Background(), "TE1", "https://example.org/")
	if err != nil {
		t.Fatalf("adopt: %v", err)
	}
	return fake, driver, session.ID
}

func TestDetachEmbeddedLeavesTheFenceAndTheTabAlone(t *testing.T) {
	fake, driver, id := adoptedEmbedded(t)
	before := len(fake.Calls())
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	found := false
	for _, call := range fake.Calls()[before:] {
		switch call.Method {
		case "Target.detachFromTarget":
			found = true
			if call.Session != "" || !containsJSON(call.Params, `"sessionId":"SE1"`) {
				t.Errorf("detach on %q params %s", call.Session, call.Params)
			}
		case "Fetch.disable", "Target.closeTarget":
			t.Errorf("%s was sent", call.Method)
		}
	}
	if !found {
		t.Fatalf("no detach: %v", fake.Methods())
	}
	if _, err := driver.lookup(id); err != nil {
		t.Errorf("session forgotten: %v", err)
	}
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Errorf("second detach = %v, want nil", err)
	}
	n := 0
	for _, call := range fake.Calls() {
		if call.Method == "Target.detachFromTarget" {
			n++
		}
	}
	if n != 1 {
		t.Errorf("detach sent %d times", n)
	}
}

func TestReattachEmbeddedUsesANewSessionWithoutNavigating(t *testing.T) {
	fake, driver, id := adoptedEmbedded(t)
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"sessionId": "SE2"}, nil
	})
	before := len(fake.Calls())
	if _, err := driver.ReattachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("reattach: %v", err)
	}
	after := fake.Calls()[before:]
	if len(after) == 0 || after[0].Method != "Target.attachToTarget" ||
		!containsJSON(after[0].Params, `"targetId":"TE1"`) || !containsJSON(after[0].Params, `"flatten":true`) {
		t.Fatalf("first call = %+v", after)
	}
	var sawEnable bool
	for _, call := range after {
		switch call.Method {
		case "Page.navigate", "Fetch.disable", "Fetch.enable", "Target.closeTarget":
			t.Errorf("%s was sent on reattach", call.Method)
		case "Page.enable":
			sawEnable = call.Session == "SE2"
		}
	}
	if !sawEnable {
		t.Errorf("Page.enable missing on SE2: %v", fake.Methods())
	}
	if _, err := driver.ReattachEmbedded(context.Background(), id); err != nil {
		t.Errorf("second reattach = %v, want nil", err)
	}

	mark := len(fake.Calls())
	_, _ = driver.Screenshot(context.Background(), id)
	used := false
	for _, call := range fake.Calls()[mark:] {
		if call.Method == "Page.captureScreenshot" {
			used = call.Session == "SE2"
		}
	}
	if !used {
		t.Error("screenshot did not go to the new cdp session")
	}
}

func TestDetachAndReattachEmbeddedUnknownSession(t *testing.T) {
	_, _, driver := connectEmbedded(t)
	if err := driver.DetachEmbedded(context.Background(), "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("detach err = %v", err)
	}
	if _, err := driver.ReattachEmbedded(context.Background(), "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("reattach err = %v", err)
	}
}

// chainEmbedded is an adopted embedded session whose main frame is F1.
func chainEmbedded(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, _, driver := connectEmbedded(t)
	fake.Handle("Page.getFrameTree", func(cdptest.Call) (any, error) {
		return map[string]any{"frameTree": map[string]any{"frame": map[string]any{"id": "F1"}}}, nil
	})
	session, err := driver.Adopt(context.Background(), "TE1", "https://example.org/")
	if err != nil {
		t.Fatalf("adopt: %v", err)
	}
	return fake, driver, session.ID
}

// emitWhileDetached raises one paused request per item and waits until each has been answered.
func emitPaused(t *testing.T, fake *cdptest.Browser, requests []map[string]any) {
	t.Helper()
	answered := func() int {
		return len(callsTo(fake, "Fetch.continueRequest")) + len(callsTo(fake, "Fetch.failRequest"))
	}
	want := answered() + len(requests)
	for _, request := range requests {
		pauseRequest(fake, "", request)
	}
	deadline := time.Now().Add(3 * time.Second)
	for answered() < want {
		if time.Now().After(deadline) {
			t.Fatalf("only %d of %d requests answered: %v", answered()-(want-len(requests)), len(requests), fake.Methods())
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func pausedAs(id, frame, url, resourceType string) map[string]any {
	request := requestStage(url, "GET", resourceType, nil)
	request["requestId"] = id
	request["frameId"] = frame
	return request
}

func reattachWith(t *testing.T, fake *cdptest.Browser, driver *Driver, id browser.SessionID, sessionID string) browser.Returned {
	t.Helper()
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"sessionId": sessionID}, nil
	})
	returned, err := driver.ReattachEmbedded(context.Background(), id)
	if err != nil {
		t.Fatalf("reattach: %v", err)
	}
	return returned
}

func TestReattachEmbeddedReturnsTheMainFrameDocumentsSeenWhileDetached(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	emitPaused(t, fake, []map[string]any{
		pausedAs("A1", "F1", "https://example.org/a", "Document"),
		pausedAs("A2", "F1", "https://example.org/a", "Document"),
		pausedAs("A3", "F2", "https://example.org/frame", "Document"),
		pausedAs("A4", "F1", "https://example.org/api", "XHR"),
		pausedAs("A5", "F1", "https://idp.example.net/login", "Document"),
	})
	// Requests are answered concurrently, so the order of the two recorded urls is only stable when
	// each is waited for; the dedupe of a repeat needs them adjacent, which they are by emission.
	returned := reattachWith(t, fake, driver, id, "SE2")
	want := []string{"https://example.org/a", "https://idp.example.net/login"}
	if !slices.Equal(returned.Chain, want) {
		t.Errorf("chain = %v, want %v", returned.Chain, want)
	}
}

func TestEmbeddedChainStopsAtReattachAndRestartsPerWindow(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	ctx := context.Background()
	if err := driver.DetachEmbedded(ctx, id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	emitPaused(t, fake, []map[string]any{pausedAs("B1", "F1", "https://example.org/first", "Document")})
	first := reattachWith(t, fake, driver, id, "SE2")
	if !slices.Equal(first.Chain, []string{"https://example.org/first"}) {
		t.Fatalf("first chain = %v", first.Chain)
	}

	emitPaused(t, fake, []map[string]any{pausedAs("B2", "F1", "https://example.org/after", "Document")})

	if err := driver.DetachEmbedded(ctx, id); err != nil {
		t.Fatalf("second detach: %v", err)
	}
	emitPaused(t, fake, []map[string]any{pausedAs("B3", "F1", "https://example.org/second", "Document")})
	second := reattachWith(t, fake, driver, id, "SE3")
	if !slices.Equal(second.Chain, []string{"https://example.org/second"}) {
		t.Errorf("second chain = %v, want only the second window", second.Chain)
	}
}

func TestReattachOfANeverDetachedSessionReturnsAnEmptyChain(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	before := len(callsTo(fake, "Target.attachToTarget"))
	returned, err := driver.ReattachEmbedded(context.Background(), id)
	if err != nil {
		t.Fatalf("reattach: %v", err)
	}
	if returned.Chain == nil || len(returned.Chain) != 0 {
		t.Errorf("chain = %#v, want non-nil empty", returned.Chain)
	}
	if n := len(callsTo(fake, "Target.attachToTarget")); n != before {
		t.Errorf("attachToTarget sent %d more times", n-before)
	}
}

func TestFailedReattachKeepsTheChainForTheRetry(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	emitPaused(t, fake, []map[string]any{pausedAs("D1", "F1", "https://example.org/kept", "Document")})
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return nil, errors.New("tab gone")
	})
	if _, err := driver.ReattachEmbedded(context.Background(), id); err == nil {
		t.Fatal("reattach succeeded")
	}
	returned := reattachWith(t, fake, driver, id, "SE2")
	if !slices.Equal(returned.Chain, []string{"https://example.org/kept"}) {
		t.Errorf("chain = %v, want the first window's", returned.Chain)
	}
}
