package adapter

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"nucleoschannelkit/contract"

	"nucleostelegram/telegram"
)

func callbackQuery(data string) telegram.CallbackQuery {
	return telegram.CallbackQuery{
		ID:   "551",
		Data: data,
		From: telegram.User{ID: 4242},
		Message: &telegram.Message{
			MessageID:       990,
			Chat:            telegram.Chat{ID: -100123, Type: "supergroup"},
			MessageThreadID: 5,
			IsTopicMessage:  true,
			// The message is the bot's own: the presser is the callback's From, not this.
			From: &telegram.User{ID: 1, IsBot: true},
		},
	}
}

func TestCallbackMatchesFixture(t *testing.T) {
	got, ok, reason := InboundCallback(callbackQuery("tk_9f2c"))
	if !ok {
		t.Fatalf("dropped: %s", reason)
	}
	want := withoutSenderName(eventFixture(t, "action"))
	want.At = nil // a callback query carries no timestamp
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("event differs:\n got %+v\nwant %+v", got, want)
	}
}

func TestCallbackPassesDecodeStrict(t *testing.T) {
	got, ok, _ := InboundCallback(callbackQuery("tk_9f2c"))
	if !ok {
		t.Fatal("dropped")
	}
	b, err := json.Marshal(got)
	if err != nil {
		t.Fatal(err)
	}
	ev, err := contract.DecodeStrict[contract.Event](b)
	if err != nil {
		t.Fatalf("DecodeStrict: %v", err)
	}
	if verr := ev.Validate(); verr != nil {
		t.Fatalf("Validate: %v", verr)
	}
}

func TestCallbackTokenUninterpreted(t *testing.T) {
	got, ok, _ := InboundCallback(callbackQuery("anything-without-prefix"))
	if !ok || *got.Action.Token != "anything-without-prefix" {
		t.Fatalf("ok=%v event=%+v", ok, got)
	}
}

func TestCallbackDataTooLongDropped(t *testing.T) {
	if _, ok, reason := InboundCallback(callbackQuery(strings.Repeat("a", 65))); ok || reason == "" {
		t.Fatalf("65-byte data must be dropped with a reason (ok=%v reason=%q)", ok, reason)
	}
	if _, ok, _ := InboundCallback(callbackQuery(strings.Repeat("a", 64))); !ok {
		t.Fatal("64-byte data is allowed")
	}
}

func TestCallbackEmptyDataDropped(t *testing.T) {
	if _, ok, reason := InboundCallback(callbackQuery("")); ok || reason == "" {
		t.Fatalf("empty data must be dropped with a reason (ok=%v reason=%q)", ok, reason)
	}
}

func TestCallbackNoMessageDropped(t *testing.T) {
	q := callbackQuery("tk_9f2c")
	q.Message = nil
	if _, ok, reason := InboundCallback(q); ok || reason == "" {
		t.Fatalf("a callback without message must be dropped with a reason (ok=%v reason=%q)", ok, reason)
	}
}
