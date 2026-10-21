// §spec pilar-de-browser

package pool

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

func TestAttachEmbeddedWaitsForTheTabToBeListed(t *testing.T) {
	view := newWebview(t)
	pool, _ := preparedPool(t)
	// Listed only after the first read has already come back empty.
	time.AfterFunc(150*time.Millisecond, func() { view.addPage("T1", "late") })

	if _, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "late")); err != nil {
		t.Fatalf("attach: %v", err)
	}
}

func TestAttachEmbeddedStopsWaitingWhenCancelled(t *testing.T) {
	view := newWebview(t)
	pool, _ := preparedPool(t)
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()

	started := time.Now()
	_, err := pool.AttachEmbedded(ctx, attachRequest(view, "never"))
	if err == nil || errors.Is(err, browser.ErrNoSuchTarget) {
		t.Fatalf("err = %v, want the context's", err)
	}
	if time.Since(started) > time.Second {
		t.Errorf("waited %v after the context ended", time.Since(started))
	}
}

func TestEmbeddedTabClosedByThePersonFreesItsSlot(t *testing.T) {
	view := newWebview(t)
	pool, _ := preparedPool(t) // max_sessions 4
	for i := 0; i < 7; i++ {
		id, nonce := fmt.Sprintf("T%d", i), fmt.Sprintf("n%d", i)
		view.addPage(id, nonce)
		if _, err := pool.AttachEmbedded(context.Background(), attachRequest(view, nonce)); err != nil {
			t.Fatalf("attach %d: %v", i, err)
		}
		// The person closes the tab: the browser detaches our CDP session.
		view.cdp.Emit("", "Target.detachedFromTarget", map[string]any{"sessionId": "SE-" + id})

		deadline := time.Now().Add(2 * time.Second)
		for {
			pool.mu.Lock()
			live := len(pool.sessions)
			pool.mu.Unlock()
			if live == 0 {
				break
			}
			if time.Now().After(deadline) {
				t.Fatalf("after closing tab %d the pool still holds %d sessions", i, live)
			}
			time.Sleep(10 * time.Millisecond)
		}
	}
}

func TestEmbeddedDetachByUsDoesNotFreeTheSession(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	pool, _ := preparedPool(t)
	session, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1"))
	if err != nil {
		t.Fatal(err)
	}
	if err := pool.DetachEmbedded(context.Background(), session.ID); err != nil {
		t.Fatal(err)
	}
	view.cdp.Emit("", "Target.detachedFromTarget", map[string]any{"sessionId": "SE-T1"})
	time.Sleep(200 * time.Millisecond)
	pool.mu.Lock()
	live := len(pool.sessions)
	pool.mu.Unlock()
	if live != 1 {
		t.Errorf("sessions = %d, want 1: our own detach must keep the session", live)
	}
}
