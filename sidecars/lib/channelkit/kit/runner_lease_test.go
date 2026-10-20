package kit

import (
	"context"
	"net/http"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// blockingAdapter returns a loopAdapter whose Execute blocks until release is
// closed (entered is closed when the first Execute starts), then succeeds.
func blockingAdapter() (ad *loopAdapter, entered chan struct{}, release func()) {
	entered = make(chan struct{})
	gate := make(chan struct{})
	var once, rel sync.Once
	ad = &loopAdapter{exec: func(ctx context.Context, _ contract.Action) ([]string, error) {
		once.Do(func() { close(entered) })
		select {
		case <-gate:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		return []string{"m1"}, nil
	}}
	return ad, entered, func() { rel.Do(func() { close(gate) }) }
}

func (g *loopRig) waitEntered(entered <-chan struct{}) {
	g.t.Helper()
	select {
	case <-entered:
	case <-time.After(10 * time.Second):
		g.t.Fatal("Execute never started")
	}
}

// waitSkipped waits (real time, bounded) until n redeliveries were skipped.
func (g *loopRig) waitSkipped(n int64) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for g.runner.Stats.Snapshot().RedeliveredSkipped < n {
		if time.Now().After(deadline) {
			g.t.Fatalf("redelivered skipped = %d, want %d", g.runner.Stats.Snapshot().RedeliveredSkipped, n)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestRunnerLeaseRenewsEvery30sDuringLongExecute(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	stop := g.start()
	g.waitEntered(entered)
	for i := 1; i <= 3; i++ {
		g.clk.BlockUntil(2) // the lease renewal timer and the parked outbox long-poll
		g.clk.Advance(30 * time.Second)
		g.waitCalls(fakecore.RouteRenew, i)
	}
	g.clk.BlockUntil(2) // renewal re-armed: the third renewal completed
	if n := g.core.Calls(fakecore.RouteRenew); n != 3 {
		t.Fatalf("renew calls = %d at 90s, want exactly 3", n)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StateLeased {
		t.Fatalf("state = %q at 90s, want %q", st, fakecore.StateLeased)
	}
	release()
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 1 {
		t.Fatalf("attempts = %d, want 1 (never redelivered)", attempts)
	}
	if got := g.runner.Stats.Snapshot().RedeliveredSkipped; got != 0 {
		t.Fatalf("redelivered skipped = %d, want 0", got)
	}
}

// expireAndRedeliver forces the first renewal to fail with 409 not_leased and
// advances 60s, so the core redelivers the id while its execution is in flight.
func expireAndRedeliver(g *loopRig, entered <-chan struct{}) {
	g.t.Helper()
	g.waitEntered(entered)
	g.core.FailNext(fakecore.RouteRenew, 1, http.StatusConflict, `{"error":"not_leased"}`)
	g.clk.BlockUntil(2)
	g.clk.Advance(30 * time.Second) // the renewal gets 409 and the lease is lost
	g.waitCalls(fakecore.RouteRenew, 1)
	g.waitCalls(fakecore.RouteOutbox, 2)
	g.clk.BlockUntil(1) // only the re-parked long-poll is left: the lease no longer renews
	g.clk.Advance(30 * time.Second)
	g.waitSkipped(1)
}

func TestRunnerLeaseExpiredInflightRedeliveryIsNotExecutedTwice(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	stop := g.start()
	expireAndRedeliver(g, entered)
	if _, attempts, _ := g.core.Delivery(id); attempts != 2 {
		t.Fatalf("attempts = %d, want 2 (the core redelivered)", attempts)
	}
	if got := g.runner.Stats.Snapshot().RedeliveredSkipped; got != 1 {
		t.Fatalf("redelivered skipped = %d, want 1", got)
	}
	release()
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
}

func TestRunnerLeaseResumesRenewingAfterInflightRedelivery(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	stop := g.start()
	expireAndRedeliver(g, entered)
	g.clk.BlockUntil(2) // resumed renewal and the long-poll
	g.clk.Advance(30 * time.Second)
	g.waitCalls(fakecore.RouteRenew, 2) // the failed one, then a real one
	g.clk.BlockUntil(2)
	if st, _, _ := g.core.Delivery(id); st != fakecore.StateLeased {
		t.Fatalf("state = %q after the resumed renewal, want %q", st, fakecore.StateLeased)
	}
	release()
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()
	if _, attempts, _ := g.core.Delivery(id); attempts > 2 {
		t.Fatalf("attempts = %d, want at most 2", attempts)
	}
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "m1" {
		t.Fatalf("platform refs = %v, want [m1]", refs)
	}
}

func TestRunnerLeaseInflightReleasedAfterPanicSoActionCanReturn(t *testing.T) {
	var mu sync.Mutex
	calls := 0
	ad := &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		mu.Lock()
		calls++
		n := calls
		mu.Unlock()
		if n == 1 {
			panic("first try blows up")
		}
		return nil, nil
	}}
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "again"))
	stop := g.start()
	g.waitCalls(fakecore.RouteAck, 1)
	g.waitInflight(0)
	g.waitState(id, fakecore.StatePending)
	// The retryable failure put it back in the queue; time brings it back.
	deadline := time.Now().Add(10 * time.Second)
	for {
		if st, _, _ := g.core.Delivery(id); st == fakecore.StateDone {
			break
		}
		if time.Now().After(deadline) {
			st, att, _ := g.core.Delivery(id)
			t.Fatalf("action never reached done: state %q attempts %d", st, att)
		}
		g.clk.Advance(RenewInterval)
		time.Sleep(time.Millisecond)
	}
	g.waitInflight(0)
	stop()
	// The fake clock keeps advancing while the retry settles, so a further
	// redelivery can race in: only "the panicked first try, then at least one
	// more execution" is guaranteed.
	if got := ad.executions(); len(got) < 2 {
		t.Fatalf("executions = %v, want at least 2 (the retry ran)", got)
	}
	mu.Lock()
	total := calls
	mu.Unlock()
	if total < 2 {
		t.Fatalf("exec calls = %d, want at least 2 (first panicked, retry ran)", total)
	}
}
