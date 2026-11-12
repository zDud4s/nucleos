package fakecore_test

import (
	"strconv"
	"strings"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

func ackPath(c *fakecore.Core, id uint64) string {
	return outboxPath(c) + "/" + strconv.FormatUint(id, 10) + "/ack"
}

func postAck(t *testing.T, c *fakecore.Core, id uint64, body string) (int, string) {
	t.Helper()
	return call(t, c, "POST", ackPath(c, id), "application/json", []byte(body))
}

// ackState posts an ack that must be answered 200 and returns the decoded answer.
func ackState(t *testing.T, c *fakecore.Core, id uint64, body string) contract.AckResponse {
	t.Helper()
	status, out := postAck(t, c, id, body)
	if status != 200 {
		t.Fatalf("ack %d %s: %d %s", id, body, status, out)
	}
	r, err := contract.DecodeAckResponse([]byte(out))
	if err != nil {
		t.Fatalf("ack response %s does not validate: %v", out, err)
	}
	return r
}

// deliver polls without waiting and returns the ids that were handed out.
func deliver(t *testing.T, c *fakecore.Core) []uint64 {
	t.Helper()
	resp := awaitPoll(t, startPoll(c, 0))
	ids := []uint64{}
	for _, raw := range resp.Actions {
		a, err := contract.ParseAction(raw)
		if err != nil {
			t.Fatalf("action %s: %v", raw, err)
		}
		ids = append(ids, a.ActionID())
	}
	return ids
}

func wantState(t *testing.T, c *fakecore.Core, id uint64, state fakecore.State, attempts int) {
	t.Helper()
	st, n, ok := c.Delivery(id)
	if !ok || st != state || n != attempts {
		t.Fatalf("delivery %d = %s/%d (known %v), want %s/%d", id, st, n, ok, state, attempts)
	}
}

const (
	okRefs        = `{"ok":true,"platform_refs":["993","994"]}`
	retryable45s  = `{"failed":{"retryable":true,"retry_after_ms":45000,"reason":"rate_limited"}}`
	retryableNone = `{"failed":{"retryable":true,"reason":"network"}}`
	permanent     = `{"failed":{"retryable":false,"reason":"thread_not_found"}}`
)

func TestAckOkMarksDoneAndStoresRefs(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)

	r := ackState(t, c, id, okRefs)
	if r.State != contract.AckDone || r.Ignored {
		t.Fatalf("response %+v, want done", r)
	}
	wantState(t, c, id, fakecore.StateDone, 1)
	if refs := c.PlatformRefs(id); strings.Join(refs, ",") != "993,994" {
		t.Fatalf("platform_refs = %v", refs)
	}
}

func TestAckOkWithoutRefsIsSuccess(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(contract.AnswerAction{Conversation: "c1", ActionRef: "r", Text: "ok"})
	deliver(t, c)
	if r := ackState(t, c, id, `{"ok":true}`); r.State != contract.AckDone {
		t.Fatalf("response %+v", r)
	}
	wantState(t, c, id, fakecore.StateDone, 1)
	if refs := c.PlatformRefs(id); len(refs) != 0 {
		t.Fatalf("platform_refs = %v, want none", refs)
	}
}

func TestAckInvalidBodyIs400WithAt(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	cases := []struct{ body, code, at string }{
		{`{"ok":false}`, "value", "/ok"},
		{`{}`, "one_of", ""},
		{`{"ok":true,"failed":{"retryable":true,"reason":"network"}}`, "one_of", ""},
		{`{"ok":true,"platform_refs":[""]}`, "empty", "/platform_refs/0"},
		{`{"failed":{"retryable":false,"retry_after_ms":1000,"reason":"thread_not_found"}}`, "forbidden", "/failed/retry_after_ms"},
		{`not json`, "", ""},
	}
	for _, tc := range cases {
		status, body := postAck(t, c, id, tc.body)
		if status != 400 || !strings.Contains(body, `"error"`) {
			t.Fatalf("%s: got %d %s, want 400", tc.body, status, body)
		}
		if tc.code != "" && (!strings.Contains(body, `"`+tc.code+`"`) || !strings.Contains(body, `"at":"`+tc.at+`"`)) {
			t.Fatalf("%s: body %s lacks error %q at %q", tc.body, body, tc.code, tc.at)
		}
	}
	wantState(t, c, id, fakecore.StateLeased, 1)
}

func TestAckUnknownAndMalformedIDs(t *testing.T) {
	c := newCore(t)
	if status, _ := postAck(t, c, 999, `{"ok":true}`); status != 404 {
		t.Fatalf("unknown id: %d, want 404", status)
	}
	if status, _ := call(t, c, "POST", outboxPath(c)+"/abc/ack", "application/json", []byte(`{"ok":true}`)); status != 400 {
		t.Fatalf("bad id: %d, want 400", status)
	}
}

func TestAckRetryableAfterDelayWinsOverBackoff(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	now := c.Clock().Now()

	r := ackState(t, c, id, retryable45s)
	if r.State != contract.AckPending || r.Ignored {
		t.Fatalf("response %+v, want pending", r)
	}
	wantState(t, c, id, fakecore.StatePending, 1)
	if due, ok := c.DueAt(id); !ok || !due.Equal(now.Add(45*time.Second)) {
		t.Fatalf("due_at = %v, want %v (the 45 s delay beats the 2 s backoff)", due, now.Add(45*time.Second))
	}

	// Not redelivered before due_at, redelivered with the same id at due_at.
	c.Clock().Advance(44 * time.Second)
	if ids := deliver(t, c); len(ids) != 0 {
		t.Fatalf("redelivered early: %v", ids)
	}
	c.Clock().Advance(time.Second)
	if ids := deliver(t, c); len(ids) != 1 || ids[0] != id {
		t.Fatalf("redelivery = %v, want [%d]", ids, id)
	}
	wantState(t, c, id, fakecore.StateLeased, 2)
}

func TestAckRetryAfterCeilingIsNotTruncated(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	now := c.Clock().Now()
	ackState(t, c, id, `{"failed":{"retryable":true,"retry_after_ms":86400000,"reason":"rate_limited"}}`)
	if due, _ := c.DueAt(id); !due.Equal(now.Add(24 * time.Hour)) {
		t.Fatalf("due_at = %v, want +24h", due)
	}
}

func TestAckBackoffGrowsAndNeverExceedsTheCeiling(t *testing.T) {
	ceiling := time.Duration(contract.MaxRetryAfterMS) * time.Millisecond
	want := []time.Duration{2 * time.Second, 4 * time.Second, 8 * time.Second}
	for i, w := range want {
		if got := fakecore.Backoff(i + 1); got != w {
			t.Fatalf("Backoff(%d) = %v, want %v", i+1, got, w)
		}
	}
	for n := 0; n <= 200; n++ {
		got := fakecore.Backoff(n)
		if got <= 0 || got > 5*time.Minute || got > ceiling {
			t.Fatalf("Backoff(%d) = %v, want within (0, 5m]", n, got)
		}
	}
	if got := fakecore.Backoff(20); got != 5*time.Minute {
		t.Fatalf("Backoff(20) = %v, want the 5 min cap", got)
	}
}

func TestAckRetryableWithoutDelayUsesBackoffOnly(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	now := c.Clock().Now()
	ackState(t, c, id, retryableNone)
	if due, _ := c.DueAt(id); !due.Equal(now.Add(2 * time.Second)) {
		t.Fatalf("1st retry due_at = %v, want +2s", due)
	}

	c.Clock().Advance(2 * time.Second)
	if ids := deliver(t, c); len(ids) != 1 {
		t.Fatalf("redelivery = %v", ids)
	}
	now = c.Clock().Now()
	ackState(t, c, id, retryableNone)
	if due, _ := c.DueAt(id); !due.Equal(now.Add(4 * time.Second)) {
		t.Fatalf("2nd retry due_at = %v, want +4s (n counts deliveries)", due)
	}
}

func TestAckRetryBlocksItsConversationButNotAnother(t *testing.T) {
	c := newCore(t)
	a1 := c.Enqueue(textSend("c1"))
	a2 := c.Enqueue(textSend("c1"))
	b1 := c.Enqueue(textSend("c2"))
	if ids := deliver(t, c); len(ids) != 2 || ids[0] != a1 || ids[1] != b1 {
		t.Fatalf("first delivery = %v, want [%d %d]", ids, a1, b1)
	}

	ackState(t, c, a1, retryableNone)
	ackState(t, c, b1, okRefs)
	b2 := c.Enqueue(textSend("c2"))
	if ids := deliver(t, c); len(ids) != 1 || ids[0] != b2 {
		t.Fatalf("while c1 is blocked got %v, want only [%d]", ids, b2)
	}
	wantState(t, c, a2, fakecore.StatePending, 0)

	c.Clock().Advance(2 * time.Second)
	if ids := deliver(t, c); len(ids) != 1 || ids[0] != a1 {
		t.Fatalf("after the backoff got %v, want a1 again (same id, before a2)", ids)
	}
	ackState(t, c, a1, okRefs)
	if ids := deliver(t, c); len(ids) != 1 || ids[0] != a2 {
		t.Fatalf("after a1 is done got %v, want [%d]", ids, a2)
	}
}

func TestAckTenthRetryableDeliveryFails(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	for n := 1; n <= fakecore.MaxAttempts; n++ {
		if ids := deliver(t, c); len(ids) != 1 {
			t.Fatalf("delivery %d = %v", n, ids)
		}
		wantState(t, c, id, fakecore.StateLeased, n)
		r := ackState(t, c, id, retryableNone)
		if n < fakecore.MaxAttempts {
			if r.State != contract.AckPending {
				t.Fatalf("delivery %d: %+v, want pending", n, r)
			}
			c.Clock().Advance(5 * time.Minute)
			continue
		}
		if r.State != contract.AckFailed || r.Ignored {
			t.Fatalf("delivery %d: %+v, want failed without ignored", n, r)
		}
	}
	wantState(t, c, id, fakecore.StateFailed, fakecore.MaxAttempts)
}

func TestAckTenthExpiredDeliveryFails(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	for n := 1; n <= fakecore.MaxAttempts; n++ {
		if ids := deliver(t, c); len(ids) != 1 {
			t.Fatalf("delivery %d = %v", n, ids)
		}
		c.Clock().Advance(fakecore.LeaseTTL)
	}
	// The expiry is applied by the next poll.
	if ids := deliver(t, c); len(ids) != 0 {
		t.Fatalf("an 11th delivery happened: %v", ids)
	}
	wantState(t, c, id, fakecore.StateFailed, fakecore.MaxAttempts)
}

func TestAckNonRetryableFailsAndUnblocksTheConversation(t *testing.T) {
	c := newCore(t)
	a1 := c.Enqueue(textSend("c1"))
	a2 := c.Enqueue(textSend("c1"))
	deliver(t, c)

	r := ackState(t, c, a1, `{"failed":{"retryable":false,"reason":"forbidden"}}`)
	if r.State != contract.AckFailed || r.Ignored {
		t.Fatalf("response %+v, want failed", r)
	}
	wantState(t, c, a1, fakecore.StateFailed, 1)
	if ids := deliver(t, c); len(ids) != 1 || ids[0] != a2 {
		t.Fatalf("after the permanent failure got %v, want [%d]", ids, a2)
	}
}

func TestAckFallbackHookEnqueuesForTheHomeConversation(t *testing.T) {
	c := newCore(t)
	var reasons []string
	c.Fallback(func(reason string, failed contract.Action) {
		reasons = append(reasons, reason)
		if s, ok := failed.(contract.Send); ok {
			c.Enqueue(textSend("home:" + s.Conversation))
		}
	})
	id := c.Enqueue(textSend("project"))
	deliver(t, c)

	if r := ackState(t, c, id, permanent); r.State != contract.AckFailed {
		t.Fatalf("response %+v", r)
	}
	if len(reasons) != 1 || reasons[0] != "thread_not_found" {
		t.Fatalf("hook reasons = %v", reasons)
	}
	wantState(t, c, id, fakecore.StateFailed, 1)
	ids := deliver(t, c)
	if len(ids) != 1 || ids[0] == id {
		t.Fatalf("fallback delivery = %v, want one new action", ids)
	}
	if st, _, ok := c.Delivery(ids[0]); !ok || st != fakecore.StateLeased {
		t.Fatalf("fallback action state = %s", st)
	}

	// A retryable failure never triggers the hook.
	id2 := c.Enqueue(textSend("project"))
	deliver(t, c)
	ackState(t, c, id2, retryableNone)
	if len(reasons) != 1 {
		t.Fatalf("hook fired for a retryable failure: %v", reasons)
	}
}

func TestAckLateOkAfterFallbackDoesNotResurrect(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("project"))
	deliver(t, c)
	ackState(t, c, id, permanent)

	r := ackState(t, c, id, okRefs)
	if r.State != contract.AckFailed || !r.Ignored {
		t.Fatalf("late ok = %+v, want failed + ignored", r)
	}
	wantState(t, c, id, fakecore.StateFailed, 1)
	if refs := c.PlatformRefs(id); len(refs) != 0 {
		t.Fatalf("late ok stored refs %v", refs)
	}
}

func TestAckLateAckOnDoneIsIgnored(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	deliver(t, c)
	ackState(t, c, id, `{"ok":true,"platform_refs":["1"]}`)

	for _, body := range []string{okRefs, retryable45s, permanent} {
		r := ackState(t, c, id, body)
		if r.State != contract.AckDone || !r.Ignored {
			t.Fatalf("late %s = %+v, want done + ignored", body, r)
		}
	}
	wantState(t, c, id, fakecore.StateDone, 1)
	if refs := c.PlatformRefs(id); strings.Join(refs, ",") != "1" {
		t.Fatalf("refs changed to %v", refs)
	}
}

func TestAckLateAckOnLeasedOrPendingIsAccepted(t *testing.T) {
	c := newCore(t)
	// Leased with an expired lease that no poll has noticed yet.
	leased := c.Enqueue(textSend("c1"))
	deliver(t, c)
	c.Clock().Advance(fakecore.LeaseTTL + time.Second)
	if r := ackState(t, c, leased, okRefs); r.State != contract.AckDone || r.Ignored {
		t.Fatalf("late ok on leased = %+v", r)
	}

	// Pending after a retryable ack: an ok still wins.
	pending := c.Enqueue(textSend("c2"))
	deliver(t, c)
	ackState(t, c, pending, retryable45s)
	wantState(t, c, pending, fakecore.StatePending, 1)
	if r := ackState(t, c, pending, okRefs); r.State != contract.AckDone || r.Ignored {
		t.Fatalf("late ok on pending = %+v", r)
	}
	wantState(t, c, pending, fakecore.StateDone, 1)
}

func TestAckHelloReleasesLeasesWithoutCountingAgain(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	other := c.Enqueue(textSend("c2"))
	deliver(t, c)
	wantState(t, c, id, fakecore.StateLeased, 1)

	status, body := call(t, c, "PUT", "/channels/"+c.Channel()+"/hello", "application/json", fixture(t, "hello/telegram-full.json"))
	if status != 200 {
		t.Fatalf("hello: %d %s", status, body)
	}
	wantState(t, c, id, fakecore.StatePending, 1)
	wantState(t, c, other, fakecore.StatePending, 1)

	if ids := deliver(t, c); len(ids) != 2 {
		t.Fatalf("redelivery after hello = %v", ids)
	}
	wantState(t, c, id, fakecore.StateLeased, 2)
}

func TestAckHelloReleaseOfTheTenthDeliveryFails(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	helloURL := "/channels/" + c.Channel() + "/hello"
	for n := 1; n <= fakecore.MaxAttempts; n++ {
		if ids := deliver(t, c); len(ids) != 1 {
			t.Fatalf("delivery %d = %v", n, ids)
		}
		if status, body := call(t, c, "PUT", helloURL, "application/json", fixture(t, "hello/telegram-full.json")); status != 200 {
			t.Fatalf("hello: %d %s", status, body)
		}
	}
	wantState(t, c, id, fakecore.StateFailed, fakecore.MaxAttempts)
}

func TestAckRouteEnforcesChannelIsolation(t *testing.T) {
	c := newCore(t)
	status, body := call(t, c, "POST", "/channels/other/outbox/1/ack", "application/json", []byte(`{"ok":true}`))
	if status != 403 || !strings.Contains(body, "channel_mismatch") {
		t.Fatalf("got %d %s", status, body)
	}
}
