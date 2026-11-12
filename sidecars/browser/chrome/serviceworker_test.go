// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"slices"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// TestAServiceWorkerScriptIsRefused is the first half of spec §11 test 6: a register() must not
// install. The request is a GET for a script from a LISTED origin — everything else about it passes
// the fence — so the Service-Worker header is the only thing that distinguishes it.
func TestAServiceWorkerScriptIsRefused(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	pauseRequest(fake, "S1", requestStage("https://example.org/sw.js", "GET", "Script",
		map[string]any{"Service-Worker": "script"}))

	call := waitForCall(t, fake, "Fetch.failRequest")
	if call.Session != "S1" {
		t.Errorf("answered on session %q", call.Session)
	}
}

// TestTheSameScriptWithoutTheHeaderPasses is its control. Without it, a fence that refused every
// script would pass the test above and break every page on the web.
func TestTheSameScriptWithoutTheHeaderPasses(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	pauseRequest(fake, "S1", requestStage("https://example.org/sw.js", "GET", "Script", nil))
	waitForCall(t, fake, "Fetch.continueRequest")
	if hasCall(fake, "Fetch.failRequest") {
		t.Error("an ordinary script was refused")
	}
}

// TestTheSweepRunsOnAPageSessionAndTheFenceOnTheBrowserSession pins an asymmetry that reads as a
// mistake and is not.
//
// The INTERCEPTION goes on the browser session: on a page session a worker's script fetch never
// appears at all. The SWEEP goes on a page session: ServiceWorker.enable does not exist on the
// browser session — real Chrome answers -32601, measured in gate/domains_test.go after this file's
// first version asserted the opposite and passed, because a fake browser answers everything.
func TestTheSweepRunsOnAPageSessionAndTheFenceOnTheBrowserSession(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	sweep := waitForCall(t, fake, "ServiceWorker.enable")
	if sweep.Session == "" {
		t.Fatal("ServiceWorker.enable went to the browser session, where Chrome does not have it")
	}
	for _, call := range fake.Calls() {
		if call.Method == "Fetch.enable" && call.Session != "" {
			t.Fatalf("the interception went to session %q, not the browser session", call.Session)
		}
	}
}

// TestARegisteredWorkerIsSweptBeforeAnythingOpens is the second half of test 6: one the PERSON
// registered in their headful window, where there is no fence because they are the one acting.
func TestARegisteredWorkerIsSweptBeforeAnythingOpens(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("ServiceWorker.enable", func(cdptest.Call) (any, error) {
		go fake.Emit("", "ServiceWorker.workerRegistrationUpdated", map[string]any{
			"registrations": []map[string]any{
				{"registrationId": "1", "scopeURL": "https://example.org/app/", "isDeleted": false},
				{"registrationId": "2", "scopeURL": "https://example.org/gone/", "isDeleted": true},
			},
		})
		return map[string]any{}, nil
	})

	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}

	var scopes []string
	unregisterAt := -1
	for i, call := range fake.Calls() {
		if call.Method != "ServiceWorker.unregister" {
			continue
		}
		if unregisterAt < 0 {
			unregisterAt = i
		}
		var params struct {
			ScopeURL string `json:"scopeURL"`
		}
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("params: %v", err)
		}
		scopes = append(scopes, params.ScopeURL)
	}

	if len(scopes) != 1 || scopes[0] != "https://example.org/app/" {
		t.Fatalf("unregistered %v, want only the live registration", scopes)
	}
	// The ORDER is the requirement, and the thing it is measured against is the NAVIGATION rather
	// than the first target: the sweep opens a blank page of its own to work from, because the
	// ServiceWorker domain does not exist on the browser session. A sweep that ran alongside the
	// navigation would leave a window in which the worker serves the page it is about to be removed
	// for.
	if navigateAt := fake.IndexOf("Page.navigate"); navigateAt >= 0 && unregisterAt > navigateAt {
		t.Errorf("the profile was swept after the navigation: %v", fake.Methods())
	}
	if !slices.Contains(fake.Methods(), "Target.closeTarget") {
		t.Error("the sweep left its scratch page open")
	}
}

// TestASweepThatFailsRefusesToOpen. Spec §6.2a's rule, applied to the one part of the fence that
// cleans up rather than blocks: a browser that could not clear the profile is a browser about to
// navigate with somebody else's code already running in it.
func TestASweepThatFailsRefusesToOpen(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("ServiceWorker.enable", func(cdptest.Call) (any, error) {
		return nil, errors.New("'ServiceWorker.enable' wasn't found")
	})

	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	_, err = driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
	if hasCall(fake, "Page.navigate") {
		t.Error("a navigation happened despite the profile not being swept")
	}
}

func TestAWorkerThatWillNotUnregisterAlsoRefusesToOpen(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("ServiceWorker.enable", func(cdptest.Call) (any, error) {
		go fake.Emit("", "ServiceWorker.workerRegistrationUpdated", map[string]any{
			"registrations": []map[string]any{
				{"registrationId": "1", "scopeURL": "https://example.org/app/", "isDeleted": false},
			},
		})
		return map[string]any{}, nil
	})
	fake.Handle("ServiceWorker.unregister", func(cdptest.Call) (any, error) {
		return nil, errors.New("no")
	})

	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if _, err := driver.Open(ctx, browser.OpenRequest{URL: "https://example.org/"}); !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
}
