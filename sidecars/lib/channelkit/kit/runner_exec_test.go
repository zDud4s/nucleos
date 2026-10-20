package kit

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// ackRecorder is a reverse proxy in front of the fake core that records the body
// of every ack, so a test can see the reason the kit sent.
type ackRecorder struct {
	mu     sync.Mutex
	bodies []string
}

func (a *ackRecorder) all() []string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return append([]string(nil), a.bodies...)
}

// recordAcks points the rig's client at a recording proxy of the fake core.
func (g *loopRig) recordAcks() *ackRecorder {
	g.t.Helper()
	target, err := url.Parse(g.core.URL())
	if err != nil {
		g.t.Fatal(err)
	}
	rec := &ackRecorder{}
	proxy := httputil.NewSingleHostReverseProxy(target)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost && strings.HasSuffix(r.URL.Path, "/ack") {
			b, _ := io.ReadAll(r.Body)
			rec.mu.Lock()
			rec.bodies = append(rec.bodies, string(b))
			rec.mu.Unlock()
			r.Body = io.NopCloser(strings.NewReader(string(b)))
		}
		proxy.ServeHTTP(w, r)
	}))
	g.t.Cleanup(srv.Close)
	g.runner.Client = NewClient(Config{BaseURL: srv.URL, Channel: g.core.Channel(), Token: g.core.Token(), Clock: g.clk})
	return rec
}

// waitCalls waits (real time, bounded) until the core saw at least n requests on route.
func (g *loopRig) waitCalls(route string, n int) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for g.core.Calls(route) < n {
		if time.Now().After(deadline) {
			g.t.Fatalf("route %s: %d calls, want at least %d", route, g.core.Calls(route), n)
		}
		time.Sleep(time.Millisecond)
	}
}

// waitInflight waits (real time, bounded) until the in-flight set has n entries.
func (g *loopRig) waitInflight(n int) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for g.runner.Inflight.Len() != n {
		if time.Now().After(deadline) {
			g.t.Fatalf("inflight = %d, want %d", g.runner.Inflight.Len(), n)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestRunnerExecPanicBecomesRetryableAdapterPanicAndKitContinues(t *testing.T) {
	first := true
	var mu sync.Mutex
	ad := &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		mu.Lock()
		boom := first
		first = false
		mu.Unlock()
		if boom {
			panic("adapter blew up")
		}
		return nil, nil
	}}
	g := newLoopRig(t, ad)
	rec := g.recordAcks()
	bad := g.core.Enqueue(loopSend("c1", "boom"))
	stop := g.start()
	g.waitCalls(fakecore.RouteAck, 1)
	g.waitState(bad, fakecore.StatePending) // retryable failure: back to pending with a due_at
	acks := rec.all()
	if len(acks) != 1 || !strings.Contains(acks[0], `"adapter_panic"`) || !strings.Contains(acks[0], `"retryable":true`) {
		t.Fatalf("acks = %v, want one retryable adapter_panic failure", acks)
	}
	// The kit survived: a later action still runs to done.
	good := g.core.Enqueue(loopSend("c2", "fine"))
	g.waitState(good, fakecore.StateDone)
	stop()
	if got := g.runner.Stats.Snapshot().Panics; got != 1 {
		t.Fatalf("panics = %d, want 1", got)
	}
	if n := g.runner.Inflight.Len(); n != 0 {
		t.Fatalf("inflight = %d after a panic, want 0", n)
	}
}

func TestRunnerExecInflightReleasedAfterError(t *testing.T) {
	ad := &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		return nil, Permanent("chat_gone")
	}}
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateFailed)
	g.waitInflight(0)
	stop()
}

func TestRunnerExecCancelledExecuteSendsNoAck(t *testing.T) {
	entered := make(chan struct{})
	ad := &loopAdapter{exec: func(ctx context.Context, _ contract.Action) ([]string, error) {
		close(entered)
		<-ctx.Done()
		return nil, context.Canceled
	}}
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	select {
	case <-entered:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute never started")
	}
	stop() // cancels the context; Execute returns context.Canceled
	if n := g.core.Calls(fakecore.RouteAck); n != 0 {
		t.Fatalf("ack calls = %d, want 0 (the lease expires and the core redelivers)", n)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StateLeased {
		t.Fatalf("state = %q, want %q", st, fakecore.StateLeased)
	}
	if n := g.runner.Inflight.Len(); n != 0 {
		t.Fatalf("inflight = %d, want 0", n)
	}
}

func TestRunnerExecAckAfterCancelStillSentWithinGrace(t *testing.T) {
	entered := make(chan struct{})
	ad := &loopAdapter{exec: func(ctx context.Context, _ contract.Action) ([]string, error) {
		close(entered)
		<-ctx.Done() // shutdown arrives mid-send; the send itself completes
		return []string{"m1"}, nil
	}}
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	select {
	case <-entered:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute never started")
	}
	stop()
	g.waitState(id, fakecore.StateDone)
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "m1" {
		t.Fatalf("platform refs = %v, want [m1]", refs)
	}
}

func TestRunnerExecCancelledRunWaitsForInflightUntilShutdownGrace(t *testing.T) {
	entered := make(chan struct{})
	release := make(chan struct{})
	ad := &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		close(entered)
		<-release // ignores ctx: a stuck adapter
		return nil, nil
	}}
	g := newLoopRig(t, ad)
	g.runner.ShutdownGrace = 7 * time.Second
	g.core.Enqueue(loopSend("c1", "hi"))
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- g.runner.Run(ctx) }()
	t.Cleanup(func() { cancel(); close(release) })
	select {
	case <-entered:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute never started")
	}
	cancel()
	g.clk.BlockUntil(2) // the lease renewal timer and the shutdown grace timer
	select {
	case err := <-done:
		t.Fatalf("Run returned %v while an execution was in flight", err)
	case <-time.After(50 * time.Millisecond):
	}
	g.clk.Advance(6 * time.Second)
	select {
	case err := <-done:
		t.Fatalf("Run returned %v before the grace elapsed", err)
	case <-time.After(50 * time.Millisecond):
	}
	g.clk.Advance(time.Second)
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v, want nil", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not return after the grace elapsed")
	}
}

func TestRunnerExecAckRetriedOnServerErrorWithSingleExecutionAndRenewal(t *testing.T) {
	ad := &loopAdapter{}
	g := newLoopRig(t, ad)
	g.core.FailNext(fakecore.RouteAck, 2, http.StatusInternalServerError, "")
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitCalls(fakecore.RouteAck, 1)
	// The first ack failed and the retry is sleeping: the entry stays in flight.
	if n := g.runner.Inflight.Len(); n != 1 {
		t.Fatalf("inflight = %d during the ack retry, want 1", n)
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		if st, _, _ := g.core.Delivery(id); st == fakecore.StateDone {
			break
		}
		if time.Now().After(deadline) {
			st, att, _ := g.core.Delivery(id)
			t.Fatalf("action never reached done: state %q attempts %d", st, att)
		}
		g.clk.Advance(RenewInterval) // fires the ack backoff and the lease renewal
		time.Sleep(time.Millisecond)
	}
	g.waitInflight(0)
	stop()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
	if n := g.core.Calls(fakecore.RouteAck); n != 3 {
		t.Fatalf("ack calls = %d, want 3 (500, 500, 200)", n)
	}
	if n := g.core.Calls(fakecore.RouteRenew); n < 1 {
		t.Fatalf("renew calls = %d, want at least 1 while the ack was being retried", n)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 1 {
		t.Fatalf("attempts = %d, want 1", attempts)
	}
}
