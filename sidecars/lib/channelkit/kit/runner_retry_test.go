package kit

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// waitHellos waits (real time, bounded) until the core accepted n hellos.
func (g *loopRig) waitHellos(n int) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		if _, got := g.core.Hello(); got >= n {
			return
		}
		if time.Now().After(deadline) {
			_, got := g.core.Hello()
			g.t.Fatalf("hellos = %d, want at least %d", got, n)
		}
		time.Sleep(time.Millisecond)
	}
}

// settle gives goroutines woken by the last core change a moment to re-arm their
// fake timers before the test advances the clock.
func (g *loopRig) settle() { time.Sleep(20 * time.Millisecond) }

// settleRound waits for the n-th ack to be applied and the action to leave the
// in-flight set, with the core holding id back in pending.
func (g *loopRig) settleRound(id uint64, n int) {
	g.t.Helper()
	g.waitCalls(fakecore.RouteAck, n)
	g.waitInflight(0)
	g.waitState(id, fakecore.StatePending)
	g.settle()
}

// runToExit runs the Runner and returns the channel its Run result arrives on;
// the run is cancelled when the test ends.
func (g *loopRig) runToExit() <-chan error {
	g.t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	finished := make(chan struct{})
	go func() { done <- g.runner.Run(ctx); close(finished) }()
	g.t.Cleanup(func() {
		cancel()
		select {
		case <-finished:
		case <-time.After(10 * time.Second):
		}
	})
	return done
}

func (g *loopRig) wantExit(done <-chan error) error {
	g.t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(10 * time.Second):
		g.t.Fatal("Run did not return")
		return nil
	}
}

// failingFirst returns an adapter whose first n executions fail with err and
// whose later ones succeed with one platform ref.
func failingFirst(n int, err error) *loopAdapter {
	var ad *loopAdapter
	ad = &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		if len(ad.executions()) <= n {
			return nil, err
		}
		return []string{"m1"}, nil
	}}
	return ad
}

func TestRunnerRetryAfterHintIsAckedOnceAndTheCoreRedeliversWhenDue(t *testing.T) {
	ad := failingFirst(1, Retryable("rate_limited", 45*time.Second))
	g := newLoopRig(t, ad)
	rec := g.recordAcks()
	start := g.clk.Now()
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.settleRound(id, 1)

	bodies := rec.all()
	if len(bodies) != 1 || !strings.Contains(bodies[0], `"retry_after_ms":45000`) {
		t.Fatalf("acks = %v, want one carrying retry_after_ms 45000", bodies)
	}
	if due, _ := g.core.DueAt(id); !due.Equal(start.Add(45 * time.Second)) {
		t.Fatalf("due_at = %v, want %v", due, start.Add(45*time.Second))
	}

	g.clk.BlockUntil(1)
	g.clk.Advance(44 * time.Second)
	g.clk.BlockUntil(1)
	g.settle()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v at 44s, want the single first one (the kit does not repeat)", got)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StatePending {
		t.Fatalf("state = %q at 44s, want %q", st, fakecore.StatePending)
	}

	g.clk.Advance(time.Second)
	g.waitState(id, fakecore.StateDone)
	stop()
	if got := ad.executions(); len(got) != 2 || got[0] != id || got[1] != id {
		t.Fatalf("executions = %v, want the same id twice", got)
	}
	if n := g.core.Calls(fakecore.RouteAck); n != 2 {
		t.Fatalf("ack calls = %d, want 2 (retry, then ok)", n)
	}
}

func TestRunnerRetryWithoutHintLeavesTimingToTheCoreBackoff(t *testing.T) {
	ad := failingFirst(3, Retryable("network", 0))
	g := newLoopRig(t, ad)
	rec := g.recordAcks()
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	for n, want := range []time.Duration{2 * time.Second, 4 * time.Second, 8 * time.Second} {
		g.settleRound(id, n+1)
		due, _ := g.core.DueAt(id)
		if got := due.Sub(g.clk.Now()); got != want || got != fakecore.Backoff(n+1) {
			t.Fatalf("delivery %d: retry in %v, want %v", n+1, got, want)
		}
		g.clk.BlockUntil(1)
		g.clk.Advance(want)
	}
	g.waitState(id, fakecore.StateDone)
	stop()
	for _, b := range rec.all() {
		if strings.Contains(b, "retry_after_ms") {
			t.Fatalf("ack %s carries retry_after_ms, want core backoff only", b)
		}
	}
	if got := g.runner.Stats.Snapshot().Executions; got != 4 {
		t.Fatalf("executions = %d, want 4", got)
	}
}

func TestRunnerRetryBlocksOnlyItsOwnConversation(t *testing.T) {
	var ad *loopAdapter
	var a1 uint64
	ad = &loopAdapter{exec: func(_ context.Context, a contract.Action) ([]string, error) {
		first := 0
		for _, id := range ad.executions() {
			if id == a1 {
				first++
			}
		}
		if a.ActionID() == a1 && first == 1 {
			return nil, Retryable("network", 0)
		}
		return []string{"m1"}, nil
	}}
	g := newLoopRig(t, ad)
	a1 = g.core.Enqueue(loopSend("A", "first"))
	a2 := g.core.Enqueue(loopSend("A", "second"))
	b1 := g.core.Enqueue(loopSend("B", "other"))
	stop := g.start()

	g.waitState(b1, fakecore.StateDone) // B is not delayed by A's retry
	g.waitCalls(fakecore.RouteAck, 2)
	g.waitInflight(0)
	g.waitState(a1, fakecore.StatePending)
	g.settle()
	if st, att, _ := g.core.Delivery(a2); st != fakecore.StatePending || att != 0 {
		t.Fatalf("a2: state %q attempts %d, want pending and never delivered", st, att)
	}
	for _, id := range ad.executions() {
		if id == a2 {
			t.Fatal("a2 executed while a1 was waiting for its retry")
		}
	}

	g.clk.BlockUntil(1)
	g.clk.Advance(2 * time.Second)
	g.waitState(a1, fakecore.StateDone)
	g.waitState(a2, fakecore.StateDone)
	stop()
	got := ad.executions()
	if len(got) != 4 || got[2] != a1 || got[3] != a2 {
		t.Fatalf("executions = %v, want a1 again and only then a2 (%d, %d)", got, a1, a2)
	}
	if _, att, _ := g.core.Delivery(a2); att != 1 {
		t.Fatalf("a2 attempts = %d, want 1", att)
	}
}

func TestRunnerRetryTenthDeliveryWithoutOkFailsTheAction(t *testing.T) {
	ad := &loopAdapter{exec: func(_ context.Context, a contract.Action) ([]string, error) {
		if s, ok := a.(contract.Send); ok && s.Conversation == "c2" {
			return []string{"m1"}, nil // the marker
		}
		return nil, Retryable("network", 0)
	}}
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "doomed"))
	stop := g.start()
	for n := 1; n < fakecore.MaxAttempts; n++ {
		g.settleRound(id, n)
		g.clk.BlockUntil(1)
		g.clk.Advance(fakecore.Backoff(n))
	}
	g.waitCalls(fakecore.RouteAck, fakecore.MaxAttempts)
	g.waitState(id, fakecore.StateFailed)
	g.waitInflight(0)
	if got := g.runner.Stats.Snapshot().Executions; got != 10 {
		t.Fatalf("executions = %d, want 10", got)
	}
	if _, att, _ := g.core.Delivery(id); att != 10 {
		t.Fatalf("attempts = %d, want 10", att)
	}

	// Long after, nothing comes back: a marker action behind it is the only new work.
	g.settle()
	g.clk.BlockUntil(1)
	g.clk.Advance(10 * time.Minute)
	marker := g.core.Enqueue(loopSend("c2", "marker"))
	g.waitState(marker, fakecore.StateDone)
	stop()
	got := ad.executions()
	if len(got) != 11 || got[10] != marker {
		t.Fatalf("executions = %v, want ten of the doomed action then the marker", got)
	}
}

func TestRunnerRetryPermanentThreadNotFoundFallsBackToTheHomeConversation(t *testing.T) {
	ad := &loopAdapter{exec: func(_ context.Context, a contract.Action) ([]string, error) {
		if s, ok := a.(contract.Send); ok && s.Conversation == "home" {
			return []string{"fb"}, nil
		}
		return nil, Permanent("thread_not_found")
	}}
	g := newLoopRig(t, ad)
	reasons := make(chan string, 1)
	var fallbackID uint64
	var mu sync.Mutex
	g.core.Fallback(func(reason string, _ contract.Action) {
		reasons <- reason
		id := g.core.Enqueue(loopSend("home", "fallback"))
		mu.Lock()
		fallbackID = id
		mu.Unlock()
	})
	id := g.core.Enqueue(loopSend("chat/thread", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateFailed)
	g.waitState(id+1, fakecore.StateDone)
	stop()
	if r := <-reasons; r != "thread_not_found" {
		t.Fatalf("failure reason = %q, want thread_not_found", r)
	}
	mu.Lock()
	defer mu.Unlock()
	if fallbackID != id+1 {
		t.Fatalf("fallback id = %d, want %d", fallbackID, id+1)
	}
	if got := ad.executions(); len(got) != 2 || got[0] != id || got[1] != id+1 {
		t.Fatalf("executions = %v, want [%d %d]", got, id, id+1)
	}
	if refs := g.core.PlatformRefs(id + 1); len(refs) != 1 || refs[0] != "fb" {
		t.Fatalf("fallback refs = %v, want [fb]", refs)
	}
}

func TestRunnerRetryHelloIsSentBeforeTheFirstPollAndAgainAfterAReconnect(t *testing.T) {
	ad := &loopAdapter{}
	g := newLoopRig(t, ad)
	var mu sync.Mutex
	var pollsAtHello []int
	g.core.OnHello(func(contract.Hello) {
		mu.Lock()
		pollsAtHello = append(pollsAtHello, g.core.Calls(fakecore.RouteOutbox))
		mu.Unlock()
	})
	stop := g.start()
	g.waitHellos(1)
	g.clk.BlockUntil(1) // the parked long-poll
	g.core.Down()
	g.clk.Advance(30 * time.Second) // the poll ends; every later request fails
	// Three failed polls (backoff 2s, 4s, 8s), then the hello (also failing: 16s).
	for _, d := range []time.Duration{2 * time.Second, 4 * time.Second, 8 * time.Second} {
		g.clk.BlockUntil(1)
		g.clk.Advance(d)
	}
	g.clk.BlockUntil(1) // the sleep after the failed hello
	g.core.Up()
	g.clk.Advance(16 * time.Second)
	g.waitHellos(2)
	id := g.core.Enqueue(loopSend("c1", "back"))
	g.waitState(id, fakecore.StateDone)
	stop()

	mu.Lock()
	defer mu.Unlock()
	if len(pollsAtHello) != 2 || pollsAtHello[0] != 0 || pollsAtHello[1] != 1 {
		t.Fatalf("outbox polls seen at each hello = %v, want [0 1] (hello precedes the first successful poll)", pollsAtHello)
	}
	if h, n := g.core.Hello(); n != 2 || h.Kind != "adapter" || h.Capabilities == nil {
		t.Fatalf("last hello %+v after %d, want kind adapter with capabilities, 2 hellos", h, n)
	}
}

func TestRunnerRetryTwoPollErrorsThenSuccessDoNotSendAnotherHello(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{})
	g.core.FailNext(fakecore.RouteOutbox, 2, http.StatusInternalServerError, `{"error":"boom"}`)
	stop := g.start()
	for _, d := range []time.Duration{2 * time.Second, 4 * time.Second} {
		g.clk.BlockUntil(1)
		g.clk.Advance(d)
	}
	id := g.core.Enqueue(loopSend("c1", "hi"))
	g.waitState(id, fakecore.StateDone)
	stop()
	if _, n := g.core.Hello(); n != 1 {
		t.Fatalf("hellos = %d, want exactly 1", n)
	}
	if n := g.core.Calls(fakecore.RouteHello); n != 1 {
		t.Fatalf("hello requests = %d, want 1", n)
	}
}

func TestRunnerRetryHelloReleasingAnInflightLeaseDoesNotExecuteItTwice(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	stop := g.start()
	g.waitEntered(entered)
	g.core.FailNext(fakecore.RouteOutbox, 3, http.StatusInternalServerError, `{"error":"boom"}`)
	g.clk.BlockUntil(2) // lease renewal and the parked long-poll
	g.clk.Advance(30 * time.Second)
	g.waitCalls(fakecore.RouteRenew, 1)
	// Three 5xx polls (backoff 2s, 4s, 8s) make the next poll follow a new hello.
	for _, d := range []time.Duration{2 * time.Second, 4 * time.Second, 8 * time.Second} {
		g.clk.BlockUntil(2) // renewal timer and the backoff sleep
		g.clk.Advance(d)
	}
	g.waitHellos(2) // released the lease; the very next poll redelivers it (t=44s)
	g.waitSkipped(1)
	if got := g.runner.Inflight.Len(); got != 1 {
		t.Fatalf("inflight = %d, want the action still tracked across the reconnect", got)
	}
	if _, att, _ := g.core.Delivery(id); att != 2 {
		t.Fatalf("attempts = %d, want 2 (hello release, then the redelivery)", att)
	}

	// The new delivery keeps being renewed: the first lease would be long gone by 120s.
	g.clk.BlockUntil(2)
	g.clk.Advance(16 * time.Second) // t=60s
	g.waitCalls(fakecore.RouteRenew, 2)
	for i := 3; i <= 4; i++ {
		g.clk.BlockUntil(2)
		g.clk.Advance(30 * time.Second)
		g.waitCalls(fakecore.RouteRenew, i)
	}
	g.clk.BlockUntil(2)
	if st, att, _ := g.core.Delivery(id); st != fakecore.StateLeased || att != 2 {
		t.Fatalf("state %q attempts %d at 120s, want leased and 2", st, att)
	}
	release()
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
	snap := g.runner.Stats.Snapshot()
	if snap.LateAcks != 0 || snap.AckRejected != 0 {
		t.Fatalf("late acks = %d, ack rejected = %d, want 0 and 0 (the final ack was accepted)", snap.LateAcks, snap.AckRejected)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "m1" {
		t.Fatalf("platform refs = %v, want [m1]", refs)
	}
}

func TestRunnerRetryFatalStatusesMakeRunReturnAnError(t *testing.T) {
	conflict := `{"error":"contract_version","contract_version":"1.0.0"}`
	cases := []struct {
		name   string
		route  string
		status int
		body   string
		send   bool // enqueue an action so the ack is reached
		want   error
	}{
		{"poll 401", fakecore.RouteOutbox, http.StatusUnauthorized, "", false, ErrFatal},
		{"poll 403", fakecore.RouteOutbox, http.StatusForbidden, "", false, ErrFatal},
		{"poll 409 contract_version", fakecore.RouteOutbox, http.StatusConflict, conflict, false, ErrFatal},
		{"ack 401", fakecore.RouteAck, http.StatusUnauthorized, "", true, ErrFatal},
		{"ack 403", fakecore.RouteAck, http.StatusForbidden, "", true, ErrFatal},
		{"ack 409 contract_version", fakecore.RouteAck, http.StatusConflict, conflict, true, ErrFatal},
		{"hello 403", fakecore.RouteHello, http.StatusForbidden, "", false, ErrFatal},
		{"hello 409 contract_version", fakecore.RouteHello, http.StatusConflict, conflict, false, ErrIncompatible},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			g := newLoopRig(t, &loopAdapter{})
			g.core.FailNext(c.route, 1, c.status, c.body)
			if c.send {
				g.core.Enqueue(loopSend("c1", "hi"))
			}
			err := g.wantExit(g.runToExit())
			if !errors.Is(err, c.want) {
				t.Fatalf("Run returned %v, want an error wrapping %v", err, c.want)
			}
			if c.route == fakecore.RouteHello {
				if n := g.core.Calls(fakecore.RouteOutbox); n != 0 {
					t.Fatalf("outbox polls = %d after a rejected hello, want 0", n)
				}
			}
		})
	}
}

func TestRunnerRetryServerErrorsOnPollBackOffAndResume(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{})
	g.core.FailNext(fakecore.RouteOutbox, 3, http.StatusInternalServerError, `{"error":"boom"}`)
	done := g.runToExit()
	for i, d := range []time.Duration{2 * time.Second, 4 * time.Second, 8 * time.Second} {
		g.waitCalls(fakecore.RouteOutbox, i+1)
		g.clk.BlockUntil(1)
		g.clk.Advance(d - time.Second)
		g.settle()
		if n := g.core.Calls(fakecore.RouteOutbox); n != i+1 {
			t.Fatalf("poll %d came before its %v backoff elapsed (%d polls)", n, d, n)
		}
		g.clk.BlockUntil(1)
		g.clk.Advance(time.Second)
	}
	select {
	case err := <-done:
		t.Fatalf("Run returned %v on 5xx, want it to keep going", err)
	default:
	}
	id := g.core.Enqueue(loopSend("c1", "after the outage"))
	g.waitState(id, fakecore.StateDone)
	if _, n := g.core.Hello(); n != 2 {
		t.Fatalf("hellos = %d, want 2 (initial, and one after three consecutive errors)", n)
	}
}
