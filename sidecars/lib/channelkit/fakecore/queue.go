package fakecore

import (
	"errors"
	"time"

	"nucleoschannelkit/contract"
)

// Outbox queue constants (spec section 6.1, decisions D-B6 and D-B9).
const (
	// LeaseTTL is how long a delivery stays leased without a renew.
	LeaseTTL = 60 * time.Second
	// ImmediateTTL is how long an immediate-lane action lives before it expires.
	ImmediateTTL = 60 * time.Second
	// MaxAttempts is the delivery count after which an action that did not end
	// in an ok ack becomes failed (Q6).
	MaxAttempts = 10
)

// State is the persisted state of one outbox entry.
type State string

// The four states of the spec.
const (
	StatePending State = "pending"
	StateLeased  State = "leased"
	StateDone    State = "done"
	StateFailed  State = "failed"
)

// Reasons recorded in entry.Reason when the queue itself fails an entry.
const (
	ReasonAttemptsExhausted = "attempts_exhausted"
	ReasonExpired           = "expired"
	ReasonPermanent         = "permanent"
)

// ErrNotLeased is returned by Renew when the entry is unknown or its lease is
// not live (the route maps it to 409 not_leased).
var ErrNotLeased = errors.New("not_leased")

// entry is one outbox row, or (Ephemeral) one typing hint that is never stored.
// Values handed out by the queue are copies; mutating them changes nothing.
type entry struct {
	ID           uint64
	Action       contract.Action // nil for an ephemeral typing entry
	Conversation string
	Lane         contract.Lane
	State        State
	Attempts     int
	PlatformRefs []string
	LeaseUntil   time.Time
	DueAt        time.Time // zero = due now
	CreatedAt    time.Time
	Reason       string // set when State is failed
	Ephemeral    bool   // typing: ID is 0, no ack
}

// queue is the pure outbox structure of the fake core: no HTTP, no goroutines,
// the clock is passed in. It is not safe for concurrent use; the caller (the
// Core, in B28b) serializes access under its own mutex.
type queue struct {
	now     func() time.Time // stamps CreatedAt of an enqueued action
	next    uint64
	entries []*entry // ascending id
	typing  []string
}

// newQueue builds an empty queue; now is read only by Enqueue.
func newQueue(now func() time.Time) *queue {
	return &queue{now: now, next: 1}
}

// Enqueue stores a new pending action and returns its id (>= 1, ascending). The
// id replaces whatever ID the action carried.
func (q *queue) Enqueue(a contract.Action) uint64 {
	id := q.next
	q.next++
	a = withID(a, id)
	q.entries = append(q.entries, &entry{
		ID:           id,
		Action:       a,
		Conversation: conversationOf(a),
		Lane:         a.Lane(),
		State:        StatePending,
		CreatedAt:    q.now(),
	})
	return id
}

// EnqueueTyping records an ephemeral typing hint; the next Take returns it once
// and forgets it.
func (q *queue) EnqueueTyping(conv string) {
	q.typing = append(q.typing, conv)
}

// Get returns a copy of the entry with that id.
func (q *queue) Get(id uint64) (entry, bool) {
	e := q.find(id)
	if e == nil {
		return entry{}, false
	}
	return snapshot(e), true
}

// Take expires what is due to expire, then delivers: every due immediate action,
// and for each conversation at most its oldest unfinished ordered action, if that
// one is pending and due. A delivered entry becomes leased for LeaseTTL and its
// attempts rises by one (D-B9). A conversation whose oldest unfinished action is
// leased with a live lease, or pending with a future due_at (retry blocking),
// yields nothing. Typing hints come last, with Ephemeral set, and are dropped.
func (q *queue) Take(now time.Time) []entry {
	q.Expire(now)
	var out []entry
	blocked := map[string]bool{}
	for _, e := range q.entries {
		if e.State != StatePending && e.State != StateLeased {
			continue
		}
		if e.Lane == contract.Immediate {
			if e.State == StatePending && !now.Before(e.DueAt) {
				out = append(out, q.deliver(e, now))
			}
			continue
		}
		if blocked[e.Conversation] {
			continue
		}
		blocked[e.Conversation] = true
		if e.State == StatePending && !now.Before(e.DueAt) {
			out = append(out, q.deliver(e, now))
		}
	}
	for _, conv := range q.typing {
		out = append(out, entry{Conversation: conv, Ephemeral: true})
	}
	q.typing = nil
	return out
}

func (q *queue) deliver(e *entry, now time.Time) entry {
	e.State = StateLeased
	e.Attempts++
	e.LeaseUntil = now.Add(LeaseTTL)
	return snapshot(e)
}

// Expire applies the passage of time: an immediate action older than ImmediateTTL
// that is not finished fails; a leased entry whose lease ran out goes back to
// pending, or to failed when it has used its MaxAttempts deliveries.
func (q *queue) Expire(now time.Time) {
	for _, e := range q.entries {
		if e.State != StatePending && e.State != StateLeased {
			continue
		}
		if e.Lane == contract.Immediate && !now.Before(e.CreatedAt.Add(ImmediateTTL)) {
			q.fail(e, ReasonExpired)
			continue
		}
		if e.State == StateLeased && !now.Before(e.LeaseUntil) {
			q.unlease(e)
		}
	}
}

// Renew extends a live lease to now+LeaseTTL and returns the new deadline. After
// the deadline (or for an entry that is not leased) it returns ErrNotLeased.
func (q *queue) Renew(id uint64, now time.Time) (time.Time, error) {
	q.Expire(now)
	e := q.find(id)
	if e == nil || e.State != StateLeased {
		return time.Time{}, ErrNotLeased
	}
	e.LeaseUntil = now.Add(LeaseTTL)
	return e.LeaseUntil, nil
}

// Complete is an ok ack: a leased or pending entry becomes done and keeps the
// platform refs. On a finished or unknown entry nothing changes and applied is
// false (a late ack); state is the entry's state after the call ("" if unknown).
func (q *queue) Complete(id uint64, refs []string) (state State, applied bool) {
	e := q.find(id)
	if e == nil {
		return "", false
	}
	if !open(e) {
		return e.State, false
	}
	e.State = StateDone
	e.PlatformRefs = append([]string(nil), refs...)
	e.LeaseUntil = time.Time{}
	return e.State, true
}

// Retry is a retryable ack: a leased or pending entry goes back to pending with
// the given due_at, same id, unless it has used its MaxAttempts deliveries, in
// which case it fails. Finished or unknown entries are left alone (applied false).
func (q *queue) Retry(id uint64, dueAt time.Time) (state State, applied bool) {
	e := q.find(id)
	if e == nil {
		return "", false
	}
	if !open(e) {
		return e.State, false
	}
	e.DueAt = dueAt
	q.unlease(e)
	return e.State, true
}

// FailPermanent is a non-retryable ack: a leased or pending entry becomes failed,
// which unblocks the rest of its conversation. Finished or unknown entries are
// left alone (applied false).
func (q *queue) FailPermanent(id uint64) (state State, applied bool) {
	e := q.find(id)
	if e == nil {
		return "", false
	}
	if !open(e) {
		return e.State, false
	}
	q.fail(e, ReasonPermanent)
	return e.State, true
}

// ReleaseAll is the hello release: every leased entry goes back to pending (or
// to failed after its MaxAttempts-th delivery) without a new increment. It
// returns how many entries were leased.
func (q *queue) ReleaseAll() int {
	n := 0
	for _, e := range q.entries {
		if e.State == StateLeased {
			n++
			q.unlease(e)
		}
	}
	return n
}

// NextWake is the earliest instant after which Take or Expire may change
// something: a lease deadline, the due_at of a pending entry, or the expiry of
// an unfinished immediate action. ok is false when nothing is scheduled.
func (q *queue) NextWake() (at time.Time, ok bool) {
	consider := func(t time.Time) {
		if t.IsZero() {
			return
		}
		if !ok || t.Before(at) {
			at, ok = t, true
		}
	}
	for _, e := range q.entries {
		if !open(e) {
			continue
		}
		if e.State == StateLeased {
			consider(e.LeaseUntil)
		} else {
			consider(e.DueAt)
		}
		if e.Lane == contract.Immediate {
			consider(e.CreatedAt.Add(ImmediateTTL))
		}
	}
	return at, ok
}

// open reports whether the entry can still be acked.
func open(e *entry) bool { return e.State == StatePending || e.State == StateLeased }

// unlease ends a delivery without an ok: back to pending, or failed once the
// attempts are used up.
func (q *queue) unlease(e *entry) {
	e.LeaseUntil = time.Time{}
	if e.Attempts >= MaxAttempts {
		q.fail(e, ReasonAttemptsExhausted)
		return
	}
	e.State = StatePending
}

func (q *queue) fail(e *entry, reason string) {
	e.State = StateFailed
	e.Reason = reason
	e.LeaseUntil = time.Time{}
}

func (q *queue) find(id uint64) *entry {
	// ids are assigned ascending from 1, so id-1 is the index.
	if id == 0 || id > uint64(len(q.entries)) {
		return nil
	}
	return q.entries[id-1]
}

func snapshot(e *entry) entry {
	c := *e
	c.PlatformRefs = append([]string(nil), e.PlatformRefs...)
	return c
}

func conversationOf(a contract.Action) string {
	switch v := a.(type) {
	case contract.Send:
		return v.Conversation
	case contract.Edit:
		return v.Conversation
	case contract.AnswerAction:
		return v.Conversation
	}
	return ""
}

func withID(a contract.Action, id uint64) contract.Action {
	switch v := a.(type) {
	case contract.Send:
		v.ID = id
		return v
	case contract.Edit:
		v.ID = id
		return v
	case contract.AnswerAction:
		v.ID = id
		return v
	}
	return a
}
