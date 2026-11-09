// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// TestMain keeps the startup waits short for every test: the fakes here have no startup window to wait
// for, and making a tab again is not what most tests are about. The tests that are about it call
// remakeTabs or tune their own driver's watch.
func TestMain(m *testing.M) {
	startupWindowWait = 20 * time.Millisecond
	openLoadWait = 20 * time.Millisecond
	openAttempts = 1
	os.Exit(m.Run())
}

// remakeTabs lets a test see the remake: up to attempts tabs are made, each given a short time to report.
// It tunes the watch itself, so it is safe once the driver's sweep is running.
func remakeTabs(tabs *tabWatch, attempts int) {
	tabs.tune(0, 0, attempts)
}

// attachingFake answers createTarget with a new target id each time (T1, T2, ...) and raises its attach
// event, like Chrome with auto-attach on. live says which of them also report their state, counting
// from 1 after mark was called; before that every tab is live, which is what the driver's own service
// worker sweep at Connect gets. It returns the ids created and the ids closed since mark, and mark.
func attachingFake(fake *cdptest.Browser, live func(n int) bool) (created, closed func() []string, mark func()) {
	var mu sync.Mutex
	var made, shut []string
	var total int
	var marked bool
	fake.Handle("Target.createTarget", func(cdptest.Call) (any, error) {
		mu.Lock()
		total++
		id := fmt.Sprintf("T%d", total)
		made = append(made, id)
		n := len(made)
		isLive := !marked || live(n)
		mu.Unlock()
		go func() {
			fake.Emit("", "Target.attachedToTarget", map[string]any{
				"sessionId":          "S" + id,
				"targetInfo":         map[string]any{"targetId": id, "type": "page", "url": ""},
				"waitingForDebugger": true,
			})
			if isLive {
				fake.Emit("", "Target.targetInfoChanged", map[string]any{
					"targetInfo": map[string]any{"targetId": id, "type": "page", "url": "about:blank"},
				})
			}
		}()
		return map[string]any{"targetId": id}, nil
	})
	fake.Handle("Target.closeTarget", func(call cdptest.Call) (any, error) {
		var p struct {
			TargetID string `json:"targetId"`
		}
		_ = json.Unmarshal(call.Params, &p)
		mu.Lock()
		shut = append(shut, p.TargetID)
		mu.Unlock()
		return map[string]any{"success": true}, nil
	})
	fake.Handle("Target.getTargetInfo", func(cdptest.Call) (any, error) {
		return map[string]any{"targetInfo": map[string]any{"url": "https://example.org/", "title": "Example"}}, nil
	})
	snapshot := func(src *[]string) func() []string {
		return func() []string {
			mu.Lock()
			defer mu.Unlock()
			return append([]string(nil), *src...)
		}
	}
	mark = func() {
		mu.Lock()
		defer mu.Unlock()
		marked = true
		made, shut = nil, nil
	}
	return snapshot(&made), snapshot(&shut), mark
}

// sweptAndClosed waits for the driver's own service worker sweep to finish, scratch page closed
// included, so that what a test counts afterwards is its own.
func sweptAndClosed(t *testing.T, fake *cdptest.Browser, driver *Driver) {
	t.Helper()
	<-driver.swept
	for deadline := time.Now().Add(5 * time.Second); fake.IndexOf("Target.closeTarget") < 0; {
		if time.Now().After(deadline) {
			t.Fatal("the sweep never closed its scratch page")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

// The agent's pages have the same startup race as the person's window: over the DevTools pipe a blank
// tab made before the browser's own window exists never loads. createPage therefore waits for the
// browser's first page to report.
func TestAgentOpenWaitsForTheBrowsersOwnWindow(t *testing.T) {
	fake, conn := dial(t)
	attachingFake(fake, func(int) bool { return true })
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	driver.tabs.tune(10*time.Second, 0, 0)

	done := make(chan error, 1)
	go func() {
		_, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
		done <- err
	}()

	time.Sleep(150 * time.Millisecond)
	if fake.IndexOf("Target.createTarget") >= 0 {
		t.Fatal("createTarget was sent before the browser had a window of its own")
	}
	// Created is not enough: the page has to have reported its state.
	fake.Emit("", "Target.targetCreated", map[string]any{
		"targetInfo": map[string]any{"targetId": "NTP", "type": "page", "url": "chrome://newtab/"},
	})
	time.Sleep(100 * time.Millisecond)
	if fake.IndexOf("Target.createTarget") >= 0 {
		t.Fatal("a freshly created page was taken for a ready startup window")
	}
	fake.Emit("", "Target.targetInfoChanged", map[string]any{
		"targetInfo": map[string]any{"targetId": "NTP", "type": "page", "title": "New Tab", "url": "chrome://newtab/"},
	})
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("open: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("open did not proceed once the window existed")
	}
	if fake.IndexOf("Target.createTarget") < 0 {
		t.Fatal("createTarget was never sent")
	}
}

func TestAgentOpenGoesAheadWhenTheBrowserShowsNoWindow(t *testing.T) {
	fake, conn := dial(t)
	attachingFake(fake, func(int) bool { return true })
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}
}

// A blank tab made in the browser's first moments can be lost without an error: it attaches and never
// reports anything. createPage must notice, drop it and make another.
func TestAgentOpenRemakesAPageThatNeverReportsAnything(t *testing.T) {
	fake, conn := dial(t)
	created, closed, mark := attachingFake(fake, func(n int) bool { return n == 2 })
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	sweptAndClosed(t, fake, driver)
	remakeTabs(driver.tabs, 3)
	mark()
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if got := created(); len(got) != 2 {
		t.Fatalf("pages created = %v, want 2 (the silent one, then a live one)", got)
	}
	if got := closed(); len(got) != 1 || got[0] != created()[0] {
		t.Fatalf("closed = %v, want only the silent first page", got)
	}
}

// The last attempt is kept whatever it does, so a browser that never reports still opens a page.
func TestAgentOpenKeepsTheLastPageWhenNoneReports(t *testing.T) {
	fake, conn := dial(t)
	created, closed, mark := attachingFake(fake, func(int) bool { return false })
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	sweptAndClosed(t, fake, driver)
	remakeTabs(driver.tabs, 3)
	mark()
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if got := created(); len(got) != 3 {
		t.Fatalf("pages created = %v, want 3", got)
	}
	if got := closed(); len(got) != 2 {
		t.Fatalf("closed = %v, want the first two", got)
	}
}
