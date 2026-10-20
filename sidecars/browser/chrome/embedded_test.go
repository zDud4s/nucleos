// §spec pilar-de-browser

package chrome

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"

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
