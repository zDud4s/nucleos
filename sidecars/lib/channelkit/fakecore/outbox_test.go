package fakecore_test

import (
	"encoding/json"
	"strconv"
	"strings"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

type pollResult struct {
	status int
	body   string
	err    error
}

func outboxPath(c *fakecore.Core) string { return "/channels/" + c.Channel() + "/outbox" }

func renewPath(c *fakecore.Core, id uint64) string {
	return outboxPath(c) + "/" + strconv.FormatUint(id, 10) + "/renew"
}

// startPoll issues a long-poll in the background and returns its result channel.
func startPoll(c *fakecore.Core, wait int) <-chan pollResult {
	out := make(chan pollResult, 1)
	go func() {
		st, body, err := try(c, "GET", outboxPath(c)+"?wait="+strconv.Itoa(wait), "", nil)
		out <- pollResult{st, body, err}
	}()
	return out
}

func awaitPoll(t *testing.T, ch <-chan pollResult) contract.OutboxResponse {
	t.Helper()
	select {
	case r := <-ch:
		if r.err != nil || r.status != 200 {
			t.Fatalf("poll: status %d err %v body %s", r.status, r.err, r.body)
		}
		resp, err := contract.DecodeOutboxResponse([]byte(r.body))
		if err != nil {
			t.Fatalf("poll body %s does not validate as outbox_response: %v", r.body, err)
		}
		return resp
	case <-time.After(5 * time.Second):
		t.Fatal("poll did not return")
	}
	return contract.OutboxResponse{}
}

func assertPending(t *testing.T, ch <-chan pollResult) {
	t.Helper()
	select {
	case r := <-ch:
		t.Fatalf("poll returned early: %+v", r)
	case <-time.After(50 * time.Millisecond):
	}
}

func textSend(conv string) contract.Send {
	return contract.Send{Conversation: conv, Parts: []contract.OutPart{{Type: contract.OutText, Text: "hi"}}}
}

func TestOutboxEmptyLongPollReturnsOnlyAfterAdvance(t *testing.T) {
	c := newCore(t)
	ch := startPoll(c, 30)
	c.Clock().BlockUntil(1)
	assertPending(t, ch)
	c.Clock().Advance(30 * time.Second)
	select {
	case r := <-ch:
		if r.status != 200 || strings.ReplaceAll(r.body, " ", "") != `{"actions":[],"ephemeral":[]}` {
			t.Fatalf("got %d %q", r.status, r.body)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("poll did not return after Advance(wait)")
	}
}

func TestOutboxZeroWaitReturnsImmediately(t *testing.T) {
	c := newCore(t)
	resp := awaitPoll(t, startPoll(c, 0))
	if len(resp.Actions) != 0 || len(resp.Ephemeral) != 0 {
		t.Fatalf("got %+v", resp)
	}
}

func TestOutboxInsertionWakesWaiterWithoutAdvance(t *testing.T) {
	c := newCore(t)
	ch := startPoll(c, 30)
	c.Clock().BlockUntil(1)
	id := c.Enqueue(textSend("c1"))
	resp := awaitPoll(t, ch)
	if len(resp.Actions) != 1 {
		t.Fatalf("got %d actions", len(resp.Actions))
	}
	a, err := contract.ParseAction(resp.Actions[0])
	if err != nil || a.ActionID() != id {
		t.Fatalf("action %v err %v, want id %d", a, err, id)
	}
	if st, n, _ := c.Delivery(id); st != fakecore.StateLeased || n != 1 {
		t.Fatalf("delivery = %s/%d, want leased/1", st, n)
	}
}

func TestOutboxTypingIsDeliveredOnce(t *testing.T) {
	c := newCore(t)
	ch := startPoll(c, 30)
	c.Clock().BlockUntil(1)
	c.EnqueueTyping("c1")
	resp := awaitPoll(t, ch)
	if len(resp.Actions) != 0 || len(resp.Ephemeral) != 1 || resp.Ephemeral[0].Conversation != "c1" ||
		resp.Ephemeral[0].Type != contract.EphemeralTyping {
		t.Fatalf("got %+v", resp)
	}
	again := startPoll(c, 5)
	c.Clock().BlockUntil(1)
	c.Clock().Advance(5 * time.Second)
	if r := awaitPoll(t, again); len(r.Ephemeral) != 0 {
		t.Fatalf("typing delivered twice: %+v", r)
	}
}

func TestOutboxLeaseExpiryWakesWaiter(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	if r := awaitPoll(t, startPoll(c, 0)); len(r.Actions) != 1 {
		t.Fatalf("first poll: %d actions", len(r.Actions))
	}
	ch := startPoll(c, 120)
	c.Clock().BlockUntil(1)
	assertPending(t, ch)
	c.Clock().Advance(fakecore.LeaseTTL)
	resp := awaitPoll(t, ch)
	if len(resp.Actions) != 1 {
		t.Fatalf("redelivery: %d actions", len(resp.Actions))
	}
	if st, n, _ := c.Delivery(id); st != fakecore.StateLeased || n != 2 {
		t.Fatalf("delivery = %s/%d, want leased/2", st, n)
	}
}

func TestOutboxImmediateExpiryWakesWaiter(t *testing.T) {
	// The route cannot create a future due_at before the ack route exists (B29),
	// so the timed wake is covered through an immediate action: it expires at
	// +60s and the parked waiter wakes to an empty answer.
	c := newCore(t)
	id := c.Enqueue(contract.AnswerAction{Conversation: "c1", ActionRef: "r", Text: "ok"})
	if r := awaitPoll(t, startPoll(c, 0)); len(r.Actions) != 1 {
		t.Fatalf("first poll: %d actions", len(r.Actions))
	}
	ch := startPoll(c, 120)
	c.Clock().BlockUntil(1)
	c.Clock().Advance(fakecore.ImmediateTTL)
	// The waiter woke at the expiry instant, re-evaluated and parked again on
	// the end of its wait.
	c.Clock().BlockUntil(1)
	if st, _, _ := c.Delivery(id); st != fakecore.StateFailed {
		t.Fatalf("state = %s, want failed", st)
	}
	c.Clock().Advance(fakecore.ImmediateTTL)
	if r := awaitPoll(t, ch); len(r.Actions) != 0 {
		t.Fatalf("expired immediate action redelivered: %+v", r)
	}
}

func TestOutboxRenewRoute(t *testing.T) {
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	awaitPoll(t, startPoll(c, 0))

	c.Clock().Advance(30 * time.Second)
	status, body := call(t, c, "POST", renewPath(c, id), "", nil)
	if status != 200 {
		t.Fatalf("renew: %d %s", status, body)
	}
	rr, err := contract.DecodeRenewResponse([]byte(body))
	if err != nil {
		t.Fatalf("renew body %s: %v", body, err)
	}
	want := c.Clock().Now().Add(fakecore.LeaseTTL).Format(time.RFC3339)
	if rr.LeaseUntil != want {
		t.Fatalf("lease_until = %s, want %s", rr.LeaseUntil, want)
	}

	c.Clock().Advance(fakecore.LeaseTTL + time.Second)
	status, body = call(t, c, "POST", renewPath(c, id), "", nil)
	wantBody := strings.Join(strings.Fields(string(fixture(t, "errors/not-leased.json"))), "")
	if status != 409 || strings.Join(strings.Fields(body), "") != wantBody {
		t.Fatalf("late renew: %d %s, want 409 not_leased", status, body)
	}
	status, _ = call(t, c, "POST", renewPath(c, 999), "", nil)
	if status != 409 {
		t.Fatalf("unknown id renew: %d, want 409", status)
	}
	status, _ = call(t, c, "POST", outboxPath(c)+"/abc/renew", "", nil)
	if status != 400 {
		t.Fatalf("bad id renew: %d, want 400", status)
	}
}

func TestOutboxResponsesValidateAsOutboxResponse(t *testing.T) {
	c := newCore(t)
	c.Enqueue(textSend("c1"))
	c.Enqueue(contract.AnswerAction{Conversation: "c2", ActionRef: "r", Text: "ok"})
	c.EnqueueTyping("c3")
	resp := awaitPoll(t, startPoll(c, 0))
	if len(resp.Actions) != 2 || len(resp.Ephemeral) != 1 {
		t.Fatalf("got %d actions, %d ephemeral", len(resp.Actions), len(resp.Ephemeral))
	}
	for _, raw := range resp.Actions {
		if _, ack, err := contract.DecodeAction(raw); err != nil || ack != nil {
			t.Fatalf("action %s: ack %v err %v", raw, ack, err)
		}
		var probe map[string]any
		if json.Unmarshal(raw, &probe) != nil || probe["id"] == nil {
			t.Fatalf("action without id: %s", raw)
		}
	}
}

func TestOutboxLeasedActionIsNotReturnedByASecondPoll(t *testing.T) {
	// Q12: a live lease keeps the action out of the next poll; attempts stays 1.
	c := newCore(t)
	id := c.Enqueue(textSend("c1"))
	if r := awaitPoll(t, startPoll(c, 0)); len(r.Actions) != 1 {
		t.Fatalf("first poll: %d actions", len(r.Actions))
	}
	second := startPoll(c, 5)
	c.Clock().BlockUntil(1)
	c.Clock().Advance(5 * time.Second)
	if r := awaitPoll(t, second); len(r.Actions) != 0 {
		t.Fatalf("second poll returned %d actions under a live lease", len(r.Actions))
	}
	if st, n, _ := c.Delivery(id); st != fakecore.StateLeased || n != 1 {
		t.Fatalf("delivery = %s/%d, want leased/1", st, n)
	}
}

func TestOutboxRoutesEnforceChannelIsolation(t *testing.T) {
	c := newCore(t)
	status, body := call(t, c, "GET", "/channels/other/outbox?wait=0", "", nil)
	if status != 403 || !strings.Contains(body, "channel_mismatch") {
		t.Fatalf("got %d %s", status, body)
	}
}
