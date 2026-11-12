package telegram

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// exApi is a fake Bot API that records the last method and JSON body and answers with result.
type exAPI struct {
	method string
	body   map[string]any
	calls  atomic.Int32
}

func newExAPI(t *testing.T, result string) (*exAPI, *Client) {
	t.Helper()
	fake := &exAPI{}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		fake.calls.Add(1)
		fake.method = r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		raw, _ := io.ReadAll(r.Body)
		fake.body = map[string]any{}
		_ = json.Unmarshal(raw, &fake.body)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true,"result":` + result + `}`))
	}))
	t.Cleanup(server.Close)
	client := New("123:SECRET-TOKEN")
	client.apiBase = server.URL
	client.sleep = func(context.Context, time.Duration) bool { return true }
	return fake, client
}

func TestSendExBodyAndMessageID(t *testing.T) {
	fake, client := newExAPI(t, `{"message_id":777}`)
	id, err := client.SendHTMLEx(context.Background(), SendParams{
		To:               Destination{ChatID: 42, ThreadID: 9},
		Text:             "<b>hi</b>",
		ParseMode:        "HTML",
		ReplyToMessageID: 5,
		Silent:           true,
		Buttons:          [][]Button{{{Text: "Go", CallbackData: "go:1"}}},
	})
	if err != nil {
		t.Fatalf("SendHTMLEx() error = %v", err)
	}
	if id != 777 {
		t.Errorf("message id = %d, want 777", id)
	}
	if fake.method != "sendMessage" {
		t.Errorf("method = %q, want sendMessage", fake.method)
	}
	if fake.body["chat_id"] != float64(42) || fake.body["message_thread_id"] != float64(9) {
		t.Errorf("destination = %#v", fake.body)
	}
	if fake.body["text"] != "<b>hi</b>" || fake.body["parse_mode"] != "HTML" {
		t.Errorf("text/parse_mode = %#v", fake.body)
	}
	if fake.body["disable_notification"] != true {
		t.Errorf("disable_notification = %#v, want true", fake.body["disable_notification"])
	}
	reply, _ := fake.body["reply_parameters"].(map[string]any)
	if reply["message_id"] != float64(5) || reply["allow_sending_without_reply"] != true {
		t.Errorf("reply_parameters = %#v", fake.body["reply_parameters"])
	}
	markup, _ := fake.body["reply_markup"].(map[string]any)
	rows, _ := markup["inline_keyboard"].([]any)
	if len(rows) != 1 {
		t.Fatalf("inline_keyboard = %#v", markup)
	}
	button := rows[0].([]any)[0].(map[string]any)
	if button["text"] != "Go" || button["callback_data"] != "go:1" {
		t.Errorf("button = %#v", button)
	}
}

func TestSendExOmitsOptionalFieldsWhenAbsent(t *testing.T) {
	fake, client := newExAPI(t, `{"message_id":1}`)
	if _, err := client.SendHTMLEx(context.Background(), SendParams{To: Destination{ChatID: 42}, Text: "x"}); err != nil {
		t.Fatalf("SendHTMLEx() error = %v", err)
	}
	for _, key := range []string{"message_thread_id", "reply_parameters", "reply_markup", "disable_notification", "parse_mode"} {
		if _, present := fake.body[key]; present {
			t.Errorf("%s present in %#v, want it omitted", key, fake.body)
		}
	}
}

func TestSendExRejectsOversizedCallbackDataBeforeSending(t *testing.T) {
	fake, client := newExAPI(t, `{"message_id":1}`)
	big := strings.Repeat("a", 65)
	_, err := client.SendHTMLEx(context.Background(), SendParams{
		To: Destination{ChatID: 42}, Text: "x",
		Buttons: [][]Button{{{Text: "Go", CallbackData: big}}},
	})
	if err == nil {
		t.Fatal("SendHTMLEx() error = nil, want a rejection")
	}
	if fake.calls.Load() != 0 {
		t.Errorf("server calls = %d, want 0", fake.calls.Load())
	}
	if _, err := NewButton("Go", big); err == nil {
		t.Error("NewButton() accepted 65 bytes of callback data")
	}
	if _, err := NewButton("Go", strings.Repeat("a", 64)); err != nil {
		t.Errorf("NewButton() rejected 64 bytes: %v", err)
	}
	// Bytes, not runes: 33 two-byte runes are 66 bytes.
	if _, err := NewButton("Go", strings.Repeat("é", 33)); err == nil {
		t.Error("NewButton() counted runes instead of bytes")
	}
}

func TestEditMessageTextBody(t *testing.T) {
	fake, client := newExAPI(t, `true`)
	err := client.EditMessageText(context.Background(), 42, 777, "new", "HTML",
		[][]Button{{{Text: "A", CallbackData: "a"}}})
	if err != nil {
		t.Fatalf("EditMessageText() error = %v", err)
	}
	if fake.method != "editMessageText" {
		t.Errorf("method = %q", fake.method)
	}
	if fake.body["chat_id"] != float64(42) || fake.body["message_id"] != float64(777) ||
		fake.body["text"] != "new" || fake.body["parse_mode"] != "HTML" {
		t.Errorf("body = %#v", fake.body)
	}
	if _, ok := fake.body["reply_markup"]; !ok {
		t.Errorf("reply_markup missing: %#v", fake.body)
	}
}

func TestEditMessageTextRejectsOversizedCallbackData(t *testing.T) {
	fake, client := newExAPI(t, `true`)
	err := client.EditMessageText(context.Background(), 42, 1, "t", "",
		[][]Button{{{Text: "A", CallbackData: strings.Repeat("a", 65)}}})
	if err == nil || fake.calls.Load() != 0 {
		t.Errorf("err = %v, calls = %d, want rejection before sending", err, fake.calls.Load())
	}
}

func TestChatActionBody(t *testing.T) {
	fake, client := newExAPI(t, `true`)
	if err := client.SendChatAction(context.Background(), Destination{ChatID: 42, ThreadID: 3}, "typing"); err != nil {
		t.Fatalf("SendChatAction() error = %v", err)
	}
	if fake.method != "sendChatAction" || fake.body["action"] != "typing" ||
		fake.body["chat_id"] != float64(42) || fake.body["message_thread_id"] != float64(3) {
		t.Errorf("method = %q body = %#v", fake.method, fake.body)
	}

	if err := client.SendChatAction(context.Background(), Destination{ChatID: 42}, "typing"); err != nil {
		t.Fatal(err)
	}
	if _, present := fake.body["message_thread_id"]; present {
		t.Errorf("message_thread_id present without a topic: %#v", fake.body)
	}
}

func TestChatActionAnswerCallbackQueryCtx(t *testing.T) {
	fake, client := newExAPI(t, `true`)
	if err := client.AnswerCallbackQueryCtx(context.Background(), "cb1", "done"); err != nil {
		t.Fatalf("AnswerCallbackQueryCtx() error = %v", err)
	}
	if fake.method != "answerCallbackQuery" || fake.body["callback_query_id"] != "cb1" || fake.body["text"] != "done" {
		t.Errorf("method = %q body = %#v", fake.method, fake.body)
	}
}

func TestSendExLongRateLimitReturnsAPIErrorWithoutSleeping(t *testing.T) {
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":120}}`))
	}))
	defer server.Close()
	client := New("123:SECRET-TOKEN")
	client.apiBase = server.URL
	slept := false
	client.sleep = func(context.Context, time.Duration) bool { slept = true; return true }

	_, err := client.SendHTMLEx(context.Background(), SendParams{To: Destination{ChatID: 1}, Text: "x"})
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.RetryAfter != 120*time.Second {
		t.Fatalf("error = %v, want APIError with RetryAfter 120s", err)
	}
	if slept || calls.Load() != 1 {
		t.Errorf("slept = %v calls = %d, want no sleep and one call", slept, calls.Load())
	}
	if strings.Contains(err.Error(), "SECRET-TOKEN") {
		t.Errorf("error leaks the token: %v", err)
	}
}

func TestSendExTransportErrorNeverContainsTheToken(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	url := server.URL
	server.Close()
	client := New("123:SECRET-TOKEN")
	client.apiBase = url
	_, err := client.SendHTMLEx(context.Background(), SendParams{To: Destination{ChatID: 1}, Text: "x"})
	if err == nil {
		t.Fatal("want a transport error")
	}
	if strings.Contains(err.Error(), "SECRET-TOKEN") {
		t.Errorf("error leaks the token: %v", err)
	}
}
