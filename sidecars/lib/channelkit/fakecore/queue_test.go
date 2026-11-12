package fakecore

import (
	"errors"
	"testing"
	"time"

	"nucleoschannelkit/contract"
)

var t0 = time.Date(2026, 10, 20, 12, 0, 0, 0, time.UTC)

func newTestQueue() *queue { return newQueue(func() time.Time { return t0 }) }

func send(conv string) contract.Action {
	return contract.Send{Conversation: conv, Parts: []contract.OutPart{{Type: contract.OutText, Text: "hi"}}}
}

func answer(conv string) contract.Action {
	return contract.AnswerAction{Conversation: conv, ActionRef: "r", Text: "ok"}
}

func ids(es []entry) []uint64 {
	out := make([]uint64, len(es))
	for i, e := range es {
		out[i] = e.ID
	}
	return out
}

func mustState(t *testing.T, q *queue, id uint64, want State) entry {
	t.Helper()
	e, ok := q.Get(id)
	if !ok || e.State != want {
		t.Fatalf("entry %d: state %q ok=%v, want %q", id, e.State, ok, want)
	}
	return e
}

func TestQueueEnqueueAssignsAscendingIDs(t *testing.T) {
	q := newTestQueue()
	a, b := q.Enqueue(send("c1")), q.Enqueue(answer("c1"))
	if a != 1 || b != 2 {
		t.Fatalf("ids = %d, %d; want 1, 2", a, b)
	}
	e, _ := q.Get(b)
	if e.Action.ActionID() != 2 || e.State != StatePending || e.Attempts != 0 || e.Conversation != "c1" {
		t.Fatalf("entry = %+v", e)
	}
}

func TestQueueTakeOneOrderedPerConversation(t *testing.T) {
	q := newTestQueue()
	first := q.Enqueue(send("c1"))
	q.Enqueue(send("c1"))
	got := q.Take(t0)
	if len(got) != 1 || got[0].ID != first {
		t.Fatalf("Take = %v, want only %d", ids(got), first)
	}
	if got[0].State != StateLeased || got[0].Attempts != 1 || !got[0].LeaseUntil.Equal(t0.Add(60*time.Second)) {
		t.Fatalf("delivered entry = %+v", got[0])
	}
}

func TestQueueDifferentConversationsInParallel(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Enqueue(send("c2"))
	q.Enqueue(send("c1"))
	if got := q.Take(t0); len(got) != 2 || got[0].ID != 1 || got[1].ID != 2 {
		t.Fatalf("Take = %v, want [1 2]", ids(got))
	}
}

func TestQueueNextOrderedAfterDone(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	second := q.Enqueue(send("c1"))
	q.Take(t0)
	if st, ok := q.Complete(1, []string{"993", "994"}); st != StateDone || !ok {
		t.Fatalf("Complete = %q %v", st, ok)
	}
	if e, _ := q.Get(1); len(e.PlatformRefs) != 2 || e.PlatformRefs[1] != "994" {
		t.Fatalf("refs = %v", e.PlatformRefs)
	}
	if got := q.Take(t0); len(got) != 1 || got[0].ID != second {
		t.Fatalf("Take = %v, want [%d]", ids(got), second)
	}
}

func TestQueueAnswerActionPassesPendingSendAndExpiresAt60s(t *testing.T) {
	q := newTestQueue()
	s := q.Enqueue(send("c1"))
	// Block the conversation with a retry whose due_at is far away.
	q.Take(t0)
	q.Retry(s, t0.Add(10*time.Minute))
	a := q.Enqueue(answer("c1"))
	got := q.Take(t0)
	if len(got) != 1 || got[0].ID != a {
		t.Fatalf("Take = %v, want only the answer %d", ids(got), a)
	}
	// An answer nobody takes expires at its 60 s.
	b := q.Enqueue(answer("c2"))
	q.Expire(t0.Add(59 * time.Second))
	mustState(t, q, b, StatePending)
	q.Expire(t0.Add(60 * time.Second))
	if e := mustState(t, q, b, StateFailed); e.Reason != ReasonExpired {
		t.Fatalf("reason = %q", e.Reason)
	}
	if got := q.Take(t0.Add(61 * time.Second)); len(got) != 0 {
		t.Fatalf("expired answer delivered: %v", ids(got))
	}
}

func TestQueueAnswerActionIsNeverRedelivered(t *testing.T) {
	q := newTestQueue()
	a := q.Enqueue(answer("c1"))
	if got := q.Take(t0); len(got) != 1 {
		t.Fatalf("first Take = %v", ids(got))
	}
	if got := q.Take(t0.Add(time.Second)); len(got) != 0 {
		t.Fatalf("leased answer redelivered: %v", ids(got))
	}
	q.Expire(t0.Add(60 * time.Second))
	mustState(t, q, a, StateFailed)
}

func TestQueueLeasedActionNotReturnedWhileLeaseLives(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Enqueue(send("c1"))
	if got := q.Take(t0); len(got) != 1 {
		t.Fatalf("first Take = %v", ids(got))
	}
	if got := q.Take(t0.Add(30 * time.Second)); len(got) != 0 {
		t.Fatalf("second Take = %v, want none (Q12)", ids(got))
	}
	if e, _ := q.Get(1); e.Attempts != 1 {
		t.Fatalf("attempts = %d, want 1", e.Attempts)
	}
}

func TestQueueLeaseExpiresAndIsRedeliveredWithAttempts2(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Take(t0)
	q.Expire(t0.Add(59 * time.Second))
	mustState(t, q, 1, StateLeased)
	q.Expire(t0.Add(60 * time.Second))
	mustState(t, q, 1, StatePending)
	now := t0.Add(61 * time.Second)
	got := q.Take(now)
	if len(got) != 1 || got[0].Attempts != 2 || !got[0].LeaseUntil.Equal(now.Add(60*time.Second)) {
		t.Fatalf("Take = %+v", got)
	}
}

func TestQueueTenthDeliveryWithoutOkFails(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	next := q.Enqueue(send("c1"))
	now := t0
	for i := 1; i <= MaxAttempts; i++ {
		got := q.Take(now)
		if len(got) != 1 || got[0].ID != 1 || got[0].Attempts != i {
			t.Fatalf("delivery %d: %+v", i, got)
		}
		now = now.Add(LeaseTTL)
		q.Expire(now)
	}
	if e := mustState(t, q, 1, StateFailed); e.Attempts != MaxAttempts || e.Reason != ReasonAttemptsExhausted {
		t.Fatalf("entry = %+v", e)
	}
	// Failing unblocks the conversation.
	if got := q.Take(now); len(got) != 1 || got[0].ID != next {
		t.Fatalf("Take = %v, want [%d]", ids(got), next)
	}
}

func TestQueueRetryAndReleaseAlsoFailOnTenthDelivery(t *testing.T) {
	q := newTestQueue()
	r, h := q.Enqueue(send("c1")), q.Enqueue(send("c2"))
	now := t0
	for i := 0; i < MaxAttempts; i++ {
		if len(q.Take(now)) != 2 {
			t.Fatalf("delivery %d: want 2 entries", i+1)
		}
		if i < MaxAttempts-1 {
			q.Retry(r, now)
			q.ReleaseAll()
		}
	}
	if st, _ := q.Retry(r, now); st != StateFailed {
		t.Fatalf("retry on 10th delivery: %q", st)
	}
	if n := q.ReleaseAll(); n != 1 {
		t.Fatalf("released %d, want 1", n)
	}
	mustState(t, q, h, StateFailed)
}

func TestQueueFutureDueAtBlocksSameConversationOnly(t *testing.T) {
	q := newTestQueue()
	first := q.Enqueue(send("c1"))
	q.Enqueue(send("c1"))
	other := q.Enqueue(send("c2"))
	q.Take(t0)
	due := t0.Add(45 * time.Second)
	if st, ok := q.Retry(first, due); st != StatePending || !ok {
		t.Fatalf("Retry = %q %v", st, ok)
	}
	if got := q.Take(t0.Add(10 * time.Second)); len(got) != 0 {
		t.Fatalf("Take before due_at = %v, want none (other leased, c1 blocked)", ids(got))
	}
	q.Complete(other, nil)
	q.Enqueue(send("c2"))
	got := q.Take(t0.Add(10 * time.Second))
	if len(got) != 1 || got[0].Conversation != "c2" {
		t.Fatalf("Take = %v, want only the c2 entry", ids(got))
	}
	got = q.Take(due)
	if len(got) != 1 || got[0].ID != first || got[0].Attempts != 2 {
		t.Fatalf("Take at due_at = %+v, want the retried %d with attempts 2", got, first)
	}
}

func TestQueueFailPermanentUnblocksConversation(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	second := q.Enqueue(send("c1"))
	q.Take(t0)
	if st, ok := q.FailPermanent(1); st != StateFailed || !ok {
		t.Fatalf("FailPermanent = %q %v", st, ok)
	}
	if got := q.Take(t0); len(got) != 1 || got[0].ID != second {
		t.Fatalf("Take = %v, want [%d]", ids(got), second)
	}
}

func TestQueueLateMutatorsOnFinishedEntryChangeNothing(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Take(t0)
	q.FailPermanent(1)
	if st, ok := q.Complete(1, []string{"x"}); st != StateFailed || ok {
		t.Fatalf("Complete on failed = %q %v", st, ok)
	}
	if st, ok := q.Retry(1, t0); st != StateFailed || ok {
		t.Fatalf("Retry on failed = %q %v", st, ok)
	}
	if e, _ := q.Get(1); len(e.PlatformRefs) != 0 {
		t.Fatalf("refs = %v", e.PlatformRefs)
	}
	if st, ok := q.Complete(99, nil); st != "" || ok {
		t.Fatalf("unknown id = %q %v", st, ok)
	}
	// A late ok on a pending (expired lease) entry is accepted.
	q.Enqueue(send("c2"))
	q.Take(t0)
	q.Expire(t0.Add(LeaseTTL))
	if st, ok := q.Complete(2, nil); st != StateDone || !ok {
		t.Fatalf("late ok on pending = %q %v", st, ok)
	}
}

func TestQueueReleaseAllKeepsAttempts(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Enqueue(send("c2"))
	q.Take(t0)
	if n := q.ReleaseAll(); n != 2 {
		t.Fatalf("released %d, want 2", n)
	}
	e := mustState(t, q, 1, StatePending)
	if e.Attempts != 1 {
		t.Fatalf("attempts after release = %d, want 1", e.Attempts)
	}
	if got := q.Take(t0); len(got) != 2 || got[0].Attempts != 2 {
		t.Fatalf("redelivery = %+v", got)
	}
}

func TestQueueRenewBeforeDeadlineExtendsAfterFails(t *testing.T) {
	q := newTestQueue()
	q.Enqueue(send("c1"))
	q.Take(t0)
	at := t0.Add(30 * time.Second)
	until, err := q.Renew(1, at)
	if err != nil || !until.Equal(at.Add(LeaseTTL)) {
		t.Fatalf("Renew = %v, %v", until, err)
	}
	// The old deadline (t0+60s) no longer expires it.
	q.Expire(t0.Add(70 * time.Second))
	mustState(t, q, 1, StateLeased)
	if _, err := q.Renew(1, until); !errors.Is(err, ErrNotLeased) {
		t.Fatalf("Renew at deadline: %v, want ErrNotLeased", err)
	}
	mustState(t, q, 1, StatePending)
	if _, err := q.Renew(99, until); !errors.Is(err, ErrNotLeased) {
		t.Fatalf("Renew unknown: %v", err)
	}
}

func TestQueueTypingIsEphemeral(t *testing.T) {
	q := newTestQueue()
	q.EnqueueTyping("c1")
	got := q.Take(t0)
	if len(got) != 1 || !got[0].Ephemeral || got[0].Conversation != "c1" || got[0].ID != 0 {
		t.Fatalf("Take = %+v", got)
	}
	if again := q.Take(t0); len(again) != 0 {
		t.Fatalf("typing returned twice: %+v", again)
	}
	if id := q.Enqueue(send("c1")); id != 1 {
		t.Fatalf("typing consumed an id: next = %d", id)
	}
}

func TestQueueNextWake(t *testing.T) {
	q := newTestQueue()
	if _, ok := q.NextWake(); ok {
		t.Fatal("empty queue has a wake")
	}
	q.Enqueue(send("c1"))
	q.Take(t0)
	if at, ok := q.NextWake(); !ok || !at.Equal(t0.Add(LeaseTTL)) {
		t.Fatalf("NextWake = %v %v", at, ok)
	}
	q.Retry(1, t0.Add(5*time.Second))
	if at, _ := q.NextWake(); !at.Equal(t0.Add(5 * time.Second)) {
		t.Fatalf("NextWake after retry = %v", at)
	}
}
