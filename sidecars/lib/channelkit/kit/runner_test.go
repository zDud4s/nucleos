package kit

import (
	"context"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// loopAdapter is a scriptable Adapter that records what it was asked to do.
type loopAdapter struct {
	exec   func(ctx context.Context, a contract.Action) ([]string, error)
	typing func(ctx context.Context, conv string) error

	mu       sync.Mutex
	executed []uint64
}

// Capabilities declares the one required field (max_text), so the Runner's hello is valid.
func (l *loopAdapter) Capabilities() contract.Capabilities {
	mt := 4096
	return contract.Capabilities{MaxText: &mt}
}

func (l *loopAdapter) Execute(ctx context.Context, a contract.Action) ([]string, error) {
	l.mu.Lock()
	l.executed = append(l.executed, a.ActionID())
	l.mu.Unlock()
	if l.exec == nil {
		return nil, nil
	}
	return l.exec(ctx, a)
}

func (l *loopAdapter) Typing(ctx context.Context, conv string) error {
	if l.typing == nil {
		return nil
	}
	return l.typing(ctx, conv)
}

func (l *loopAdapter) executions() []uint64 {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]uint64(nil), l.executed...)
}

// loopRig is a Runner against the fake core, started on demand.
type loopRig struct {
	t      *testing.T
	core   *fakecore.Core
	clk    *clock.Fake
	ad     *loopAdapter
	runner *Runner
}

func newLoopRig(t *testing.T, ad *loopAdapter) *loopRig {
	t.Helper()
	clk := clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC))
	core := fakecore.New(t, clk)
	client := NewClient(Config{BaseURL: core.URL(), Channel: core.Channel(), Token: core.Token(), Clock: clk})
	return &loopRig{t: t, core: core, clk: clk, ad: ad, runner: &Runner{
		Client: client, Adapter: ad, Clock: clk, Inflight: NewInflight(), Stats: &Stats{},
	}}
}

// start runs the Runner and returns a stop function that cancels it and checks
// that Run returned cleanly.
func (g *loopRig) start() (stop func()) {
	g.t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- g.runner.Run(ctx) }()
	var once sync.Once
	stop = func() {
		once.Do(func() {
			cancel()
			select {
			case err := <-done:
				if err != nil {
					g.t.Errorf("Run returned %v after cancel", err)
				}
			case <-time.After(10 * time.Second):
				g.t.Error("Run did not return after cancel")
			}
		})
	}
	g.t.Cleanup(stop)
	return stop
}

// waitState waits (real time, bounded) until the core records state for id.
func (g *loopRig) waitState(id uint64, want fakecore.State) {
	g.t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		if st, _, ok := g.core.Delivery(id); ok && st == want {
			return
		}
		if time.Now().After(deadline) {
			st, att, _ := g.core.Delivery(id)
			g.t.Fatalf("action %d: state %q (attempts %d), want %q", id, st, att, want)
		}
		time.Sleep(time.Millisecond)
	}
}

func loopSend(conv, text string) contract.Send {
	return contract.Send{Conversation: conv, Parts: []contract.OutPart{{Type: contract.OutText, Text: text}}}
}

func TestRunnerLoopSendOkIsDoneWithPlatformRefs(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		return []string{"m1", "m2"}, nil
	}})
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateDone)
	stop()
	refs := g.core.PlatformRefs(id)
	if len(refs) != 2 || refs[0] != "m1" || refs[1] != "m2" {
		t.Fatalf("platform refs = %v, want [m1 m2]", refs)
	}
	if got := g.runner.Stats.Snapshot().Executions; got != 1 {
		t.Fatalf("executions = %d, want 1", got)
	}
	if n := g.runner.Inflight.Len(); n != 0 {
		t.Fatalf("inflight = %d after Run, want 0", n)
	}
}

func TestRunnerLoopAnswerActionOkWithoutRefs(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{})
	id := g.core.Enqueue(contract.AnswerAction{Conversation: "c1", ActionRef: "ref", Text: "ok"})
	stop := g.start()
	g.waitState(id, fakecore.StateDone)
	stop()
	if refs := g.core.PlatformRefs(id); len(refs) != 0 {
		t.Fatalf("platform refs = %v, want none", refs)
	}
}

func TestRunnerLoopSendOkWithoutRefsIsDoneAndNotRedelivered(t *testing.T) {
	ad := &loopAdapter{} // Execute returns nil, nil
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateDone)
	// A second action behind it proves the loop kept going after the first ack.
	id2 := g.core.Enqueue(loopSend("c1", "again"))
	g.waitState(id2, fakecore.StateDone)
	stop()
	if got := ad.executions(); len(got) != 2 || got[0] != id || got[1] != id2 {
		t.Fatalf("executions = %v, want [%d %d] (no redelivery)", got, id, id2)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 1 {
		t.Fatalf("attempts = %d, want 1", attempts)
	}
	if rej := g.runner.Stats.Snapshot().AckRejected; rej != 0 {
		t.Fatalf("ack rejected = %d, want 0", rej)
	}
}

func TestRunnerLoopTypingReachesAdapterWithoutAckAndDoesNotBlockPoll(t *testing.T) {
	got := make(chan string, 1)
	release := make(chan struct{})
	ad := &loopAdapter{typing: func(ctx context.Context, conv string) error {
		got <- conv
		select { // blocks until released: the poll must go on meanwhile
		case <-release:
		case <-ctx.Done():
		}
		return nil
	}}
	g := newLoopRig(t, ad)
	g.core.EnqueueTyping("c9")
	stop := g.start()
	select {
	case conv := <-got:
		if conv != "c9" {
			t.Fatalf("typing conversation = %q, want c9", conv)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("typing never reached the adapter")
	}
	id := g.core.Enqueue(loopSend("c1", "hi"))
	g.waitState(id, fakecore.StateDone) // typing is still blocked here
	close(release)
	stop()
	if n := g.core.Calls(fakecore.RouteAck); n != 1 {
		t.Fatalf("ack calls = %d, want 1 (the send only, typing has none)", n)
	}
}

func TestRunnerLoopInvalidActionIsRescuedAndOthersRun(t *testing.T) {
	ad := &loopAdapter{}
	g := newLoopRig(t, ad)
	bad := g.core.Enqueue(contract.Send{Conversation: "c1"}) // nil parts: unreadable
	good := g.core.Enqueue(loopSend("c2", "hi"))
	stop := g.start()
	g.waitState(good, fakecore.StateDone)
	g.waitState(bad, fakecore.StateFailed)
	stop()
	if got := ad.executions(); len(got) != 1 || got[0] != good {
		t.Fatalf("executions = %v, want only [%d]", got, good)
	}
}

func TestRunnerLoopRescueAckReasonIsInvalidAction(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{})
	reasons := make(chan string, 1)
	g.core.Fallback(func(reason string, _ contract.Action) { reasons <- reason })
	bad := g.core.Enqueue(contract.Send{Conversation: "c1"})
	stop := g.start()
	g.waitState(bad, fakecore.StateFailed)
	stop()
	select {
	case r := <-reasons:
		if r != contract.ReasonInvalidAction {
			t.Fatalf("rescue reason = %q, want %q", r, contract.ReasonInvalidAction)
		}
	default:
		t.Fatal("core saw no failure reason")
	}
}

func TestRunnerLoopTwoConversationsRunInParallel(t *testing.T) {
	var entered sync.WaitGroup
	entered.Add(2)
	both := make(chan struct{})
	go func() { entered.Wait(); close(both) }()
	ad := &loopAdapter{exec: func(ctx context.Context, _ contract.Action) ([]string, error) {
		entered.Done()
		select { // neither returns until both are inside Execute
		case <-both:
			return nil, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}}
	g := newLoopRig(t, ad)
	a := g.core.Enqueue(loopSend("c1", "one"))
	b := g.core.Enqueue(loopSend("c2", "two"))
	stop := g.start()
	g.waitState(a, fakecore.StateDone)
	g.waitState(b, fakecore.StateDone)
	stop()
}

func TestRunnerLoopPollNetworkErrorBacksOffAndResumes(t *testing.T) {
	ad := &loopAdapter{}
	g := newLoopRig(t, ad)
	g.core.Down()
	stop := g.start()
	g.clk.BlockUntil(1) // the backoff timer: no other timer exists while the core is down
	g.core.Up()
	id := g.core.Enqueue(loopSend("c1", "hi"))
	g.clk.Advance(2 * time.Second) // backoff.Outbox.Base
	g.waitState(id, fakecore.StateDone)
	stop()
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
}

func TestRunnerLoopGuardVetoSkipsExecute(t *testing.T) {
	ad := &loopAdapter{}
	g := newLoopRig(t, ad)
	g.runner.Guard = func(contract.Capabilities, contract.Action) *contract.Ack {
		return &contract.Ack{Failed: &contract.Failed{Retryable: false, Reason: "unsupported_capability"}}
	}
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateFailed)
	stop()
	if got := ad.executions(); len(got) != 0 {
		t.Fatalf("executions = %v, want none", got)
	}
}
