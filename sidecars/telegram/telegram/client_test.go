package telegram

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

func TestClientGetUpdates(t *testing.T) {
	const token = "test-token"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("method = %s, want POST", r.Method)
		}
		if r.URL.Path != "/bot"+token+"/getUpdates" {
			t.Errorf("path = %q, want %q", r.URL.Path, "/bot"+token+"/getUpdates")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true,"result":[{"update_id":10,"message":{"message_id":1,"chat":{"id":42},"text":"hi"}}]}`))
	}))
	defer server.Close()

	client := New(token)
	client.apiBase = server.URL

	updates, err := client.GetUpdates(0, 50)
	if err != nil {
		t.Fatalf("GetUpdates() error = %v", err)
	}
	if len(updates) != 1 {
		t.Fatalf("len(updates) = %d, want 1", len(updates))
	}
	if updates[0].UpdateID != 10 {
		t.Errorf("UpdateID = %d, want 10", updates[0].UpdateID)
	}
	if updates[0].Message == nil {
		t.Fatal("Message = nil, want a message")
	}
	if updates[0].Message.Chat.ID != 42 {
		t.Errorf("Message.Chat.ID = %d, want 42", updates[0].Message.Chat.ID)
	}
	if updates[0].Message.Text != "hi" {
		t.Errorf("Message.Text = %q, want %q", updates[0].Message.Text, "hi")
	}
}

func TestClientSendMessage(t *testing.T) {
	var request map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Errorf("decode request: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true,"result":{}}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL

	if err := client.SendMessage(42, "hello"); err != nil {
		t.Fatalf("SendMessage() error = %v", err)
	}
	if request["chat_id"] != float64(42) {
		t.Errorf("chat_id = %#v, want 42", request["chat_id"])
	}
	if request["text"] != "hello" {
		t.Errorf("text = %#v, want %q", request["text"], "hello")
	}
}

func TestClientSendMessageWithButtons(t *testing.T) {
	var request map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Errorf("decode request: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true,"result":{}}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL

	rows := [][]Button{{{Text: "Approve", CallbackData: "approve:7"}}}
	if err := client.SendMessageWithButtons(42, "proposal", rows); err != nil {
		t.Fatalf("SendMessageWithButtons() error = %v", err)
	}

	replyMarkup, ok := request["reply_markup"].(map[string]any)
	if !ok {
		t.Fatalf("reply_markup = %#v, want object", request["reply_markup"])
	}
	keyboard, ok := replyMarkup["inline_keyboard"].([]any)
	if !ok || len(keyboard) == 0 {
		t.Fatalf("inline_keyboard = %#v, want non-empty array", replyMarkup["inline_keyboard"])
	}
	firstRow, ok := keyboard[0].([]any)
	if !ok || len(firstRow) == 0 {
		t.Fatalf("first row = %#v, want non-empty array", keyboard[0])
	}
	firstButton, ok := firstRow[0].(map[string]any)
	if !ok {
		t.Fatalf("first button = %#v, want object", firstRow[0])
	}
	if firstButton["callback_data"] != "approve:7" {
		t.Errorf("callback_data = %#v, want %q", firstButton["callback_data"], "approve:7")
	}
}

func TestClientTelegramError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":false,"description":"boom"}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL

	err := client.SendMessage(42, "hello")
	if err == nil {
		t.Fatal("SendMessage() error = nil, want error")
	}
	if !strings.Contains(err.Error(), "boom") {
		t.Errorf("error = %q, want it to contain %q", err, "boom")
	}
}

// The bot token is a path segment of every request URL, and Go reports a transport failure as a
// *url.Error whose Error() prints that whole URL. Transport failures are routine here — the update
// loop holds a 50s long poll open, so a DNS blip, a reset or a timeout happens often — and each one
// used to be logged verbatim, writing the token to stderr and to whatever captures it.
func TestATransportFailureNeverPutsTheBotTokenInTheErrorText(t *testing.T) {
	const token = "8000000:AAH-this-is-the-secret"
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	// Closed on purpose: the port stops accepting, which is the failure shape being pinned.
	server.Close()

	client := New(token)
	client.apiBase = server.URL

	_, err := client.GetUpdates(0, 1)
	if err == nil {
		t.Fatal("GetUpdates() error = nil, want a transport error")
	}
	if strings.Contains(err.Error(), token) {
		t.Errorf("GetUpdates() error = %q, want the bot token redacted out of it", err)
	}

	_, err = client.DownloadFile("voice/file.ogg")
	if err == nil {
		t.Fatal("DownloadFile() error = nil, want a transport error")
	}
	if strings.Contains(err.Error(), token) {
		t.Errorf("DownloadFile() error = %q, want the bot token redacted out of it", err)
	}

	var urlErr *url.Error
	if !errors.As(err, &urlErr) {
		t.Errorf("DownloadFile() error = %v, want the transport error still reachable by errors.As", err)
	}
}

// Telegram answers a throttled sender with 429 and says how long to wait. Retrying that wait is the
// difference between an approval prompt arriving late and never arriving at all.
func TestARateLimitedRequestWaitsTheAdvertisedDelayAndRetries(t *testing.T) {
	var calls int
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		w.Header().Set("Content-Type", "application/json")
		if calls == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 2","parameters":{"retry_after":2}}`))
			return
		}
		_, _ = w.Write([]byte(`{"ok":true,"result":{}}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL
	var waited time.Duration
	client.sleep = func(_ context.Context, d time.Duration) bool {
		waited = d
		return true
	}

	if err := client.SendMessage(42, "hello"); err != nil {
		t.Fatalf("SendMessage() error = %v, want the retry to succeed", err)
	}
	if calls != 2 {
		t.Errorf("server calls = %d, want 2 (the throttled one and the retry)", calls)
	}
	if waited != 2*time.Second {
		t.Errorf("waited = %s, want the 2s Telegram asked for", waited)
	}
}

// A throttle that outlives the retries must be recognisable as a throttle: the caller has to know
// that sending something else immediately is the one thing that makes it worse.
func TestAThrottleThatNeverClearsIsReportedAsRateLimiting(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":1}}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL
	client.sleep = func(context.Context, time.Duration) bool { return true }

	err := client.SendHTML(42, "<b>hi</b>")
	if !IsRateLimited(err) {
		t.Errorf("IsRateLimited(%v) = false, want true", err)
	}
	if IsRateLimited(errors.New("invalid entities")) {
		t.Error("IsRateLimited(a plain error) = true, want false")
	}
}

// A rejection Telegram itself issued (bad HTML entities, say) is worth answering differently from a
// request that never arrived, so the two must stay distinguishable.
func TestATelegramRejectionIsNotMistakenForATransportFailure(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte(`{"ok":false,"description":"Bad Request: can't parse entities"}`))
	}))
	defer server.Close()

	client := New("token")
	client.apiBase = server.URL

	err := client.SendHTML(42, "<b>hi")
	if err == nil {
		t.Fatal("SendHTML() error = nil, want a rejection")
	}
	if IsTransport(err) {
		t.Errorf("IsTransport(%v) = true, want false — Telegram answered", err)
	}
	if IsRateLimited(err) {
		t.Errorf("IsRateLimited(%v) = true, want false", err)
	}
	if !strings.Contains(err.Error(), "can't parse entities") {
		t.Errorf("error = %q, want Telegram's description kept", err)
	}
}

func TestGetUpdatesStopsWhenItsContextIsCancelled(t *testing.T) {
	// The handler holds the poll open the way Telegram does; `release` is only there so the test
	// server can be shut down afterwards.
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	defer server.Close()
	defer close(release)

	client := New("token")
	client.apiBase = server.URL

	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()

	if _, err := client.GetUpdatesContext(ctx, 0, 50); err == nil {
		t.Fatal("GetUpdatesContext() error = nil, want the cancellation reported")
	}
}
