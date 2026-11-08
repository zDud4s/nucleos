package kit

import (
	"context"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// Contract 1.1: the ack of an action echoes the lease generation it was handed out
// under, so the core can tell a late ack of an expired lease from the current one.

func (g *loopRig) wantAckGeneration(id, want uint64) {
	g.t.Helper()
	if got, ok := g.core.AckGeneration(id); !ok || got != want {
		g.t.Fatalf("ack generation of %d = %d (acked %v), want %d", id, got, ok, want)
	}
	if n := g.core.StaleAcks(); n != 0 {
		g.t.Fatalf("stale acks = %d, want 0", n)
	}
}

func TestRunnerAckEchoesTheLeaseGeneration(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		return []string{"m1"}, nil
	}})
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateDone)
	stop()
	g.wantAckGeneration(id, 1)
}

func TestRunnerGuardVetoEchoesTheLeaseGeneration(t *testing.T) {
	g := newLoopRig(t, &loopAdapter{})
	g.runner.Guard = func(contract.Capabilities, contract.Action) *contract.Ack {
		return &contract.Ack{Failed: &contract.Failed{Retryable: false, Reason: "unsupported_capability"}}
	}
	id := g.core.Enqueue(loopSend("c1", "hi"))
	stop := g.start()
	g.waitState(id, fakecore.StateFailed)
	stop()
	g.wantAckGeneration(id, 1)
}

// A redelivery skipped while the first execution is in flight leased the row
// afresh: the ack of that execution must carry the new generation, or the core
// refuses it as stale and the action is executed a second time.
func TestRunnerAckAfterInflightRedeliveryCarriesTheNewGeneration(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	stop := g.start()
	expireAndRedeliver(g, entered)
	if gen, _ := g.core.Generation(id); gen != 2 {
		t.Fatalf("lease generation = %d, want 2 (the core redelivered)", gen)
	}
	release()
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()
	g.wantAckGeneration(id, 2)
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
}
