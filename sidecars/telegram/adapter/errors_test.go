package adapter

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"testing"
	"time"

	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

func TestClassify(t *testing.T) {
	api := func(status int, desc string) error {
		return &telegram.APIError{Method: "sendMessage", StatusCode: status, Description: desc}
	}
	cases := []struct {
		name      string
		err       error
		retryable bool
		reason    string
		after     time.Duration
	}{
		{"rate limited", &telegram.APIError{Method: "sendMessage", StatusCode: 429, RetryAfter: 45 * time.Second}, true, "rate_limited", 45 * time.Second},
		{"rate limited without hint", api(429, "Too Many Requests"), true, "rate_limited", 0},
		{"transport", &url.Error{Op: "Post", URL: "x", Err: errors.New("connection reset")}, true, "network", 0},
		{"5xx", api(502, "Bad Gateway"), true, "telegram_5xx", 0},
		{"thread not found", api(400, "Bad Request: message thread not found"), false, "thread_not_found", 0},
		{"chat not found", api(400, "Bad Request: CHAT NOT FOUND"), false, "conversation_not_found", 0},
		{"kicked", api(403, "Forbidden: bot was kicked from the supergroup chat"), false, "conversation_not_found", 0},
		{"unauthorized", api(401, "Unauthorized"), false, "unauthorized", 0},
		{"forbidden", api(403, "Forbidden: bot can't initiate conversation with a user"), false, "forbidden", 0},
		{"too long", api(400, "Bad Request: message is too long"), false, "message_too_long", 0},
		{"cant parse entities", api(400, "Bad Request: can't parse entities: Can't find end of the entity"), false, "invalid_action", 0},
		{"other 4xx", api(404, "Not Found"), false, "invalid_action", 0},
		{"wrapped api error", fmt.Errorf("send: %w", api(401, "Unauthorized")), false, "unauthorized", 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var ee *kit.ExecError
			if !errors.As(Classify(tc.err), &ee) {
				t.Fatalf("Classify(%v) is not an *ExecError", tc.err)
			}
			if ee.Retryable != tc.retryable || ee.Reason != tc.reason || ee.RetryAfter != tc.after {
				t.Fatalf("got retryable=%v reason=%q after=%v, want %v %q %v",
					ee.Retryable, ee.Reason, ee.RetryAfter, tc.retryable, tc.reason, tc.after)
			}
		})
	}
}

func TestClassifyContextErrorsPropagateUnchanged(t *testing.T) {
	for _, base := range []error{context.Canceled, context.DeadlineExceeded} {
		wrapped := &url.Error{Op: "Post", URL: "x", Err: base}
		got := Classify(wrapped)
		if got != error(wrapped) {
			t.Fatalf("Classify(%v) = %v, want the error unchanged", wrapped, got)
		}
		var ee *kit.ExecError
		if errors.As(got, &ee) {
			t.Fatalf("context error was classified: %v", got)
		}
	}
	if Classify(nil) != nil {
		t.Fatal("Classify(nil) must be nil")
	}
}

func TestClassifyIsParseEntities(t *testing.T) {
	yes := &telegram.APIError{StatusCode: 400, Description: "Bad Request: Can't parse entities: bad"}
	no := &telegram.APIError{StatusCode: 400, Description: "Bad Request: message is too long"}
	if !IsParseEntities(yes) || !IsParseEntities(fmt.Errorf("w: %w", yes)) {
		t.Fatal("parse-entities error not recognised")
	}
	if IsParseEntities(no) || IsParseEntities(errors.New("x")) || IsParseEntities(nil) {
		t.Fatal("unrelated error recognised as parse-entities")
	}
}
