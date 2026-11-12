package adapter

import (
	"context"
	"testing"
	"time"

	"nucleoschannelkit/contract"

	"nucleostelegram/telegram"
)

// TestPortDestinationFromKey ports v1 TestChatKey and TestDestinationFromKey: the conversation ref
// the adapter emits inbound ("<chat>" or "<chat>:<thread>") is read back by parseDestination, and
// the pair are inverses.
func TestPortDestinationFromKey(t *testing.T) {
	for _, tc := range []struct {
		name string
		key  string
		want telegram.Destination
	}{
		{"a topic of a group", "-1001234567890:7", telegram.Destination{ChatID: -1001234567890, ThreadID: 7}},
		{"no topic", "-1001234567890", telegram.Destination{ChatID: -1001234567890}},
		{"a positive id", "12345:3", telegram.Destination{ChatID: 12345, ThreadID: 3}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := parseDestination(tc.key)
			if !ok || got != tc.want {
				t.Fatalf("parseDestination(%q) = %+v, %v; want %+v", tc.key, got, ok, tc.want)
			}
		})
	}
}

// TestPortAKeyThatCannotBeReadIsRefusedRatherThanGuessedAt ports the v1 test of the same name.
func TestPortAKeyThatCannotBeReadIsRefusedRatherThanGuessedAt(t *testing.T) {
	for _, key := range []string{"", "abc", "-100:", ":7", "-100:abc", "-100:7:9", " -100", "-100:0"} {
		t.Run(key, func(t *testing.T) {
			if got, ok := parseDestination(key); ok {
				t.Errorf("parseDestination(%q) = %+v, true; want a refusal", key, got)
			}
		})
	}
}

// TestPortAReplyGoesBackToTheTopicItCameFrom ports v1 TestAReplyGoesBackToTheTopicItCameFrom: the
// conversation ref of an inbound topic message, used verbatim as the destination of a send, lands
// in that topic.
func TestPortAReplyGoesBackToTheTopicItCameFrom(t *testing.T) {
	u := telegram.Update{UpdateID: 1, Message: &telegram.Message{
		MessageID:       10,
		Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
		From:            &telegram.User{ID: 7},
		MessageThreadID: 7,
		IsTopicMessage:  true,
		Text:            "olá",
	}}
	ev, _, ok := Inbound(u)
	if !ok || ev.Conversation == nil || ev.Conversation.Ref == nil {
		t.Fatalf("inbound dropped or without conversation: %+v", ev)
	}
	bot := &fakeSendBot{}
	s := contract.Send{ID: 9, Conversation: *ev.Conversation.Ref, Parts: []contract.OutPart{{Type: contract.OutText, Text: "resposta"}}}
	if _, err := New(bot, nil).Execute(context.Background(), s); err != nil {
		t.Fatal(err)
	}
	if len(bot.calls) != 1 || bot.calls[0].To != (telegram.Destination{ChatID: -100123, ThreadID: 7}) {
		t.Fatalf("calls = %+v, want one send to the topic it came from", bot.calls)
	}
}

// TestPortAThrottledReplyIsNotImmediatelyResentAsPlainText ports the v1 test of the same name: a 429
// on the HTML send is returned as retryable, and no plain-text send follows it.
func TestPortAThrottledReplyIsNotImmediatelyResentAsPlainText(t *testing.T) {
	s := contract.Send{ID: 5, Conversation: "42", Parts: []contract.OutPart{{Type: contract.OutText, Text: "**bold**"}}}
	bot := &fakeSendBot{errs: map[int]error{0: &telegram.APIError{Method: "sendMessage", StatusCode: 429, RetryAfter: 5 * time.Second}}}
	if _, err := New(bot, nil).Execute(context.Background(), s); err == nil {
		t.Fatal("a throttled send must report an error")
	}
	if len(bot.calls) != 1 {
		t.Fatalf("calls = %d, want 1: a throttled send is not resent as plain text", len(bot.calls))
	}
}
