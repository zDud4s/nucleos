package telegram

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
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
