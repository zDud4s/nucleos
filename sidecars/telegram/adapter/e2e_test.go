package adapter

import (
	"context"
	"net/http"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"

	"nucleostelegram/telegram"
)

// homeChat is the private chat every scenario talks to.
const homeChat = "4242"

// TestE2EInboundMessageAdvancesOffset: a Telegram message becomes an event the core accepts, and only
// then does the update offset move.
func TestE2EInboundMessageAdvancesOffset(t *testing.T) {
	g := newE2ERig(t)
	g.start(nil)

	u := privateText(10, 1, "hello core")
	g.bot.push(u)

	evs := g.events(1)
	want, _, ok := Inbound(u)
	if !ok {
		t.Fatal("Inbound dropped the update")
	}
	if evs[0].EventID == nil || *evs[0].EventID != *want.EventID {
		t.Fatalf("accepted event id = %v, want %v", evs[0].EventID, *want.EventID)
	}
	if c := evs[0].Conversation; c == nil || c.Ref == nil || *c.Ref != homeChat {
		t.Fatalf("conversation = %+v, want ref %q", c, homeChat)
	}
	eventually(t, "offset to advance", func() bool { return g.savedOffset() == 11 })
	if bad := g.core.InvalidEvents(); len(bad) != 0 {
		t.Fatalf("core rejected events: %v", bad)
	}
	// The poll loop asks for the next update from the new offset.
	eventually(t, "getUpdates at the new offset", func() bool {
		g.bot.mu.Lock()
		defer g.bot.mu.Unlock()
		return len(g.bot.offsets) >= 2 && g.bot.offsets[len(g.bot.offsets)-1] == 11
	})
}

// TestE2ECoreFailuresRepostSameEventAndHoldOffset: the core answers 500 twice; the event is posted again
// (backoff on the shared fake clock) and the offset moves only once it was accepted.
func TestE2ECoreFailuresRepostSameEventAndHoldOffset(t *testing.T) {
	g := newE2ERig(t)
	g.core.FailNext(fakecore.RouteEvents, 2, http.StatusInternalServerError, `{"error":"boom"}`)
	g.start(nil)

	u := privateText(30, 7, "retry me")
	g.bot.push(u)

	for k := 1; k <= 2; k++ {
		g.waitCalls(fakecore.RouteEvents, k)
		g.pending(2) // the parked outbox long-poll and the post backoff
		if got := g.savedOffset(); got != 0 {
			t.Fatalf("offset = %d after %d failed posts, want 0", got, k)
		}
		if n := len(g.core.Events()); n != 0 {
			t.Fatalf("%d events accepted after %d failures, want 0", n, k)
		}
		g.clk.Advance(time.Minute)
	}

	evs := g.events(1)
	want, _, _ := Inbound(u)
	if *evs[0].EventID != *want.EventID {
		t.Fatalf("accepted event id = %q, want the original %q", *evs[0].EventID, *want.EventID)
	}
	eventually(t, "offset to advance", func() bool { return g.savedOffset() == 31 })
	if n := g.core.Calls(fakecore.RouteEvents); n != 3 {
		t.Fatalf("events calls = %d, want 3 (two failures, one success)", n)
	}
	if n := len(g.core.Events()); n != 1 {
		t.Fatalf("accepted events = %d, want exactly 1", n)
	}
}

// TestE2ERateLimitBecomesRetryAfterAndRedelivers: a Bot API 429 with retry_after 45 turns into a retryable
// ack with retry_after_ms 45000; the core holds the action back for 45 s of fake time, then it is sent
// again and completes.
func TestE2ERateLimitBecomesRetryAfterAndRedelivers(t *testing.T) {
	g := newE2ERig(t)
	g.bot.sendFn = func(n int, _ telegram.SendParams) (int64, error) {
		if n == 1 {
			return 0, &telegram.APIError{
				Method: "sendMessage", StatusCode: http.StatusTooManyRequests,
				Description: "Too Many Requests: retry after 45", RetryAfter: 45 * time.Second,
			}
		}
		return 9001, nil
	}
	id := g.core.Enqueue(e2eText(homeChat, "slow down"))
	start := g.clk.Now()
	g.start(nil)

	g.bot.waitSignal(t, 1)
	g.waitState(id, fakecore.StatePending)
	due, ok := g.core.DueAt(id)
	if !ok || !due.Equal(start.Add(45*time.Second)) {
		t.Fatalf("due_at = %v (ok=%v), want %v: retry_after_ms was not 45000", due, ok, start.Add(45*time.Second))
	}

	g.pending(1) // only the parked long-poll
	g.clk.Advance(44 * time.Second)
	g.pending(1)
	if n := g.bot.sendCount(); n != 1 {
		t.Fatalf("sends = %d at 44 s, want 1 (the core must hold the action back)", n)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StatePending {
		t.Fatalf("state = %q at 44 s, want pending", st)
	}
	// Cross the 45 s mark. The long-poll re-parks on its own goroutine and may arm its timer relative to
	// a clock reading taken just before the Advance above, so it can expire up to one long-poll wait
	// (30 s) after 45 s; the step covers that without caring which of the two happened.
	g.pending(1)
	g.clk.Advance(31 * time.Second)
	// The core records the ack before the kit releases the action's in-flight slot, so a redelivery that
	// lands in that gap is skipped and the core holds the row leased with no renewal. That cannot be
	// ordered against the fake clock, so recover instead of waiting: while the row sits leased and nothing
	// was sent again, let the lease lapse so the core re-leases and redelivers it.
	for i := 0; ; i++ {
		deadline := time.Now().Add(100 * time.Millisecond)
		for time.Now().Before(deadline) {
			if st, _, _ := g.core.Delivery(id); st == fakecore.StateDone {
				break
			}
			time.Sleep(time.Millisecond)
		}
		st, _, _ := g.core.Delivery(id)
		if st == fakecore.StateDone {
			break
		}
		if i >= 20 {
			t.Fatalf("state = %q after %d lease lapses, want done", st, i)
		}
		if st == fakecore.StateLeased && g.bot.sendCount() == 1 {
			g.clk.Advance(fakecore.LeaseTTL + time.Second)
		}
	}
	if n := g.bot.sendCount(); n != 2 {
		t.Fatalf("sends = %d, want 2", n)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "9001" {
		t.Fatalf("platform refs = %v, want [9001]", refs)
	}
	// A skipped redelivery that was re-leased counts as one more delivery attempt but sends nothing.
	if _, attempts, _ := g.core.Delivery(id); attempts < 2 {
		t.Fatalf("attempts = %d, want at least 2", attempts)
	}
}

// TestE2EThreadNotFoundFallsBackToHome: Telegram refusing a topic ("message thread not found") is a
// permanent ack with reason thread_not_found; the core's fallback re-sends the action to the home
// conversation, where it succeeds.
func TestE2EThreadNotFoundFallsBackToHome(t *testing.T) {
	g := newE2ERig(t)
	g.bot.sendFn = func(_ int, p telegram.SendParams) (int64, error) {
		if p.To.ThreadID != 0 {
			return 0, &telegram.APIError{
				Method: "sendMessage", StatusCode: http.StatusBadRequest,
				Description: "Bad Request: message thread not found",
			}
		}
		return 9100, nil
	}
	var (
		mu       sync.Mutex
		reasons  []string
		fallback uint64
	)
	g.core.Fallback(func(reason string, failed contract.Action) {
		send, ok := failed.(contract.Send)
		if !ok {
			t.Errorf("fallback action is %T, want contract.Send", failed)
			return
		}
		send.Conversation = homeChat
		id := g.core.Enqueue(send)
		mu.Lock()
		reasons, fallback = append(reasons, reason), id
		mu.Unlock()
	})
	id := g.core.Enqueue(e2eText(homeChat+":77", "into a deleted topic"))
	g.start(nil)

	g.bot.waitSignal(t, 2)
	g.waitState(id, fakecore.StateFailed)
	mu.Lock()
	gotReasons, home := append([]string(nil), reasons...), fallback
	mu.Unlock()
	if len(gotReasons) != 1 || gotReasons[0] != "thread_not_found" {
		t.Fatalf("fallback reasons = %v, want [thread_not_found]", gotReasons)
	}
	if st, attempts, _ := g.core.Delivery(id); st != fakecore.StateFailed || attempts != 1 {
		t.Fatalf("topic action: state %q attempts %d, want failed after one delivery (not retryable)", st, attempts)
	}
	g.waitState(home, fakecore.StateDone)
	sends := g.bot.sent()
	if len(sends) != 2 || sends[0].To.ChatID != 4242 || sends[0].To.ThreadID != 77 ||
		sends[1].To.ChatID != 4242 || sends[1].To.ThreadID != 0 {
		t.Fatalf("sends = %+v, want the topic first, then the home chat", sends)
	}
	if sends[0].Text != sends[1].Text {
		t.Fatalf("fallback text %q differs from the original %q", sends[1].Text, sends[0].Text)
	}
}

// TestE2ELongSendRenewsLeaseThreeTimes: a send that takes 100 s of fake time renews its lease every 30 s
// (three times), is never redelivered, and ends done.
func TestE2ELongSendRenewsLeaseThreeTimes(t *testing.T) {
	g := newE2ERig(t)
	gate := make(chan struct{})
	g.bot.sendFn = func(int, telegram.SendParams) (int64, error) {
		<-gate
		return 9200, nil
	}
	id := g.core.Enqueue(e2eText(homeChat, "a very slow upload"))
	g.start(nil)

	eventually(t, "the send to start", func() bool { return g.bot.sendCount() == 1 })
	for i := 1; i <= 3; i++ {
		g.pending(2) // the lease renewal timer and the parked long-poll
		g.clk.Advance(30 * time.Second)
		g.waitCalls(fakecore.RouteRenew, i)
	}
	g.pending(2) // the third renewal completed and re-armed
	g.clk.Advance(10 * time.Second)
	if n := g.core.Calls(fakecore.RouteRenew); n != 3 {
		t.Fatalf("renew calls = %d at 100 s, want exactly 3", n)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StateLeased {
		t.Fatalf("state = %q at 100 s, want leased", st)
	}
	close(gate)
	g.waitState(id, fakecore.StateDone)
	if n := g.bot.sendCount(); n != 1 {
		t.Fatalf("sends = %d, want 1 (never redelivered)", n)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 1 {
		t.Fatalf("attempts = %d, want 1", attempts)
	}
}

// TestE2ECrashBetweenSendAndAckResends: the process dies after Telegram took the message but before the
// ack. The core still holds the lease; the next process says hello, the lease is released, the action is
// delivered again and the duplicate send is accepted.
func TestE2ECrashBetweenSendAndAckResends(t *testing.T) {
	g := newE2ERig(t)
	g.bot.sendFn = func(int, telegram.SendParams) (int64, error) {
		g.kill() // the process is gone before it can ack...
		return 0, context.Canceled
	}
	id := g.core.Enqueue(e2eText(homeChat, "delivered twice"))
	g.start(nil)

	g.bot.waitSignal(t, 1)
	g.waitKilled()
	if st, attempts, _ := g.core.Delivery(id); st != fakecore.StateLeased || attempts != 1 {
		t.Fatalf("after the crash: state %q attempts %d, want leased after one delivery", st, attempts)
	}
	if n := g.core.Calls(fakecore.RouteAck); n != 0 {
		t.Fatalf("ack calls = %d, want none", n)
	}

	second := newE2EBot()
	g.start(second)
	g.waitState(id, fakecore.StateDone)
	sent := second.sent()
	if len(sent) != 1 || sent[0].Text != "delivered twice" || sent[0].To.ChatID != 4242 {
		t.Fatalf("resend = %+v, want the same text to the same chat", sent)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 2 {
		t.Fatalf("attempts = %d, want 2", attempts)
	}
	if _, hellos := g.core.Hello(); hellos != 2 {
		t.Fatalf("hellos = %d, want 2 (one per process)", hellos)
	}
}

// TestE2ETypingCallsSendChatAction: an ephemeral typing hint from the core reaches the Bot API as a
// "typing" chat action in the right chat.
func TestE2ETypingCallsSendChatAction(t *testing.T) {
	g := newE2ERig(t)
	g.start(nil)
	eventually(t, "hello", func() bool { _, n := g.core.Hello(); return n == 1 })
	g.core.EnqueueTyping(homeChat)

	g.bot.waitSignal(t, 1)
	g.bot.mu.Lock()
	acts := append([]actionCall(nil), g.bot.actions...)
	g.bot.mu.Unlock()
	if len(acts) != 1 || acts[0].action != "typing" || acts[0].to.ChatID != 4242 || acts[0].to.ThreadID != 0 {
		t.Fatalf("chat actions = %+v, want one typing in chat 4242", acts)
	}
	if n := g.bot.sendCount(); n != 0 {
		t.Fatalf("sends = %d, want 0: typing is not a message", n)
	}
}

// TestE2EButtonPressRoundTrip: a button press reaches the core as an action event; the core answers it with
// an answer_action, which the adapter turns into answerCallbackQuery.
func TestE2EButtonPressRoundTrip(t *testing.T) {
	g := newE2ERig(t)
	g.start(nil)

	token := "approve:7"
	g.bot.push(telegram.Update{UpdateID: 20, CallbackQuery: &telegram.CallbackQuery{
		ID:   "cb-1",
		Data: token,
		From: telegram.User{ID: 4242},
		Message: &telegram.Message{
			MessageID: 55, Chat: telegram.Chat{ID: 4242, Type: "private"},
		},
	}})

	ev := g.events(1)[0]
	if ev.Type == nil || *ev.Type != "action" || ev.Action == nil ||
		ev.Action.Token == nil || *ev.Action.Token != token || ev.Action.Ref == nil || *ev.Action.Ref != "cb-1" {
		t.Fatalf("event = %+v, want an action event carrying token %q and ref cb-1", ev, token)
	}
	eventually(t, "offset to advance", func() bool { return g.savedOffset() == 21 })

	id := g.core.Enqueue(contract.AnswerAction{Conversation: homeChat, ActionRef: *ev.Action.Ref, Text: "approved"})
	g.bot.waitSignal(t, 1)
	g.waitState(id, fakecore.StateDone)
	g.bot.mu.Lock()
	answers := append([]answerCall(nil), g.bot.answers...)
	g.bot.mu.Unlock()
	if len(answers) != 1 || answers[0].id != "cb-1" || answers[0].text != "approved" {
		t.Fatalf("answers = %+v, want answerCallbackQuery(cb-1, approved)", answers)
	}
}
