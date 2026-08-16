package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

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

// TestTheFenceGoesOnTheBrowserSessionForWorkers restates the spike's most consequential finding as a
// regression test on the one call that proves it. On the page session the script request never
// appears at all and the worker installs regardless — a fence that looked identical from every call
// site and enforced nothing.
func TestServiceWorkersAreEnabledOnTheBrowserSession(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	call := waitForCall(t, fake, "ServiceWorker.enable")
	if call.Session != "" {
		t.Fatalf("ServiceWorker.enable went to session %q, not the browser session", call.Session)
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
	// The ORDER is the requirement. A sweep that happened alongside the navigation would leave a
	// window in which the worker serves the page it is about to be removed for.
	if createAt := fake.IndexOf("Target.createTarget"); unregisterAt > createAt {
		t.Errorf("the profile was swept after a target existed: %v", fake.Methods())
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
	if hasCall(fake, "Target.createTarget") {
		t.Error("a target was created despite the profile not being swept")
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
