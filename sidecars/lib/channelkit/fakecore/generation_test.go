package fakecore_test

import (
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// deliverActions polls without waiting and returns the parsed actions.
func deliverActions(t *testing.T, c *fakecore.Core) []contract.Action {
	t.Helper()
	resp := awaitPoll(t, startPoll(c, 0))
	out := []contract.Action{}
	for _, raw := range resp.Actions {
		a, err := contract.ParseAction(raw)
		if err != nil {
			t.Fatalf("action %s: %v", raw, err)
		}
		out = append(out, a)
	}
	return out
}

func TestEveryLeaseCarriesANewGeneration(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	for want := uint64(1); want <= 3; want++ {
		got := deliverActions(t, c)
		if len(got) != 1 || got[0].ActionID() != id || got[0].LeaseGeneration() != want {
			t.Fatalf("delivery %d = %+v, want id %d with generation %d", want, got, id, want)
		}
		if g, _ := c.Generation(id); g != want {
			t.Fatalf("Generation = %d, want %d", g, want)
		}
		c.Clock().Advance(fakecore.LeaseTTL)
	}
}

// The race of rel-channels#1: A's lease expires, B leases the row again, then A's
// late retryable failure arrives. It must not put B's row back to pending.
func TestAckLateRetryableOfAnExpiredLeaseDoesNotRependTheReleasedRow(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c) // A: generation 1
	c.Clock().Advance(fakecore.LeaseTTL)
	if got := deliverActions(t, c); len(got) != 1 || got[0].LeaseGeneration() != 2 {
		t.Fatalf("redelivery = %+v, want generation 2", got) // B
	}

	status, body := postAck(t, c, id, `{"failed":{"retryable":true,"reason":"network"},"generation":1}`)
	if status != 409 {
		t.Fatalf("stale ack = %d %s, want 409", status, body)
	}
	if !strings.Contains(body, `"not_leased"`) {
		t.Fatalf("stale ack body = %s, want not_leased", body)
	}
	wantState(t, c, id, fakecore.StateLeased, 2)
	if n := c.StaleAcks(); n != 1 {
		t.Fatalf("StaleAcks = %d, want 1", n)
	}

	// B's own ack still settles it.
	if r := ackState(t, c, id, `{"ok":true,"generation":2}`); r.State != contract.AckDone || r.Ignored {
		t.Fatalf("current ack = %+v, want done", r)
	}
	wantState(t, c, id, fakecore.StateDone, 2)
}

func TestAckWithoutGenerationSkipsTheCheck(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	c.Clock().Advance(fakecore.LeaseTTL)
	deliver(t, c)
	if r := ackState(t, c, id, okRefs); r.State != contract.AckDone || r.Ignored {
		t.Fatalf("legacy ack = %+v, want done", r)
	}
	if n := c.StaleAcks(); n != 0 {
		t.Fatalf("StaleAcks = %d, want 0", n)
	}
}

func TestAckStaleGenerationOnSettledRowIsALateAck(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	ackState(t, c, id, `{"ok":true,"generation":1}`)
	if r := ackState(t, c, id, `{"ok":true,"generation":7}`); r.State != contract.AckDone || !r.Ignored {
		t.Fatalf("late ack = %+v, want done + ignored", r)
	}
	if n := c.StaleAcks(); n != 0 {
		t.Fatalf("StaleAcks = %d, want 0", n)
	}
}
