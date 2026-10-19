// §spec pilar-de-browser

package pool

import (
	"context"
	"sync"
	"sync/atomic"
	"testing"
)

// The cancel func is the one observable every instance owns without a live Chromium behind it, so it
// stands in for "the teardown body ran".

func TestInstanceStopTwiceClosesOnce(t *testing.T) {
	var calls int32
	instance := &chromeInstance{cancel: func() { atomic.AddInt32(&calls, 1) }}

	instance.stop(context.Background())
	instance.stop(context.Background())

	if got := atomic.LoadInt32(&calls); got != 1 {
		t.Fatalf("chromeInstance teardown ran %d times across two stop calls, want 1", got)
	}
}

func TestHumanInstanceStopTwiceClosesOnce(t *testing.T) {
	var calls int32
	instance := &humanInstance{cancel: func() { atomic.AddInt32(&calls, 1) }}

	instance.stop(context.Background())
	instance.stop(context.Background())

	if got := atomic.LoadInt32(&calls); got != 1 {
		t.Fatalf("humanInstance teardown ran %d times across two stop calls, want 1", got)
	}
}

func TestInstanceConcurrentStopsCloseOnce(t *testing.T) {
	var calls int32
	instance := &chromeInstance{cancel: func() { atomic.AddInt32(&calls, 1) }}

	var group sync.WaitGroup
	for i := 0; i < 8; i++ {
		group.Add(1)
		go func() {
			defer group.Done()
			instance.Shutdown(context.Background())
		}()
	}
	group.Wait()

	if got := atomic.LoadInt32(&calls); got != 1 {
		t.Fatalf("chromeInstance teardown ran %d times across 8 concurrent stops, want 1", got)
	}
}
