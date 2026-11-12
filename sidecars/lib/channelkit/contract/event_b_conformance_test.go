package contract_test

import (
	"encoding/json"
	"errors"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("event_response", func(b []byte) (any, error) {
		v, err := contract.DecodeEventResponse(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

func TestEventResponseAdvance(t *testing.T) {
	for _, f := range []string{"events/response-accepted.json", "events/response-duplicate.json", "events/response-dropped.json"} {
		r, err := contract.DecodeEventResponse(readFixture(t, f))
		if err != nil {
			t.Fatalf("%s: %v", f, err)
		}
		if !r.Advance() {
			t.Errorf("%s: Advance() = false, want true", f)
		}
	}
	if (contract.EventResponse{}).Advance() {
		t.Error("a zero EventResponse must not advance")
	}
}

func TestEventResponseEncodesOnlyTheSetFlag(t *testing.T) {
	b, err := json.Marshal(contract.EventResponse{Duplicate: true})
	if err != nil || string(b) != `{"duplicate":true}` {
		t.Fatalf("got %s, %v", b, err)
	}
}

func TestEventResponseErrors(t *testing.T) {
	for file, want := range map[string]contract.Error{
		"events/response-two-flags.json": {Code: contract.CodeOneOf, At: ""},
		"events/response-empty.json":     {Code: contract.CodeOneOf, At: ""},
		"events/response-false.json":     {Code: contract.CodeValue, At: "/accepted"},
	} {
		_, err := contract.DecodeEventResponse(readFixture(t, file))
		var ce *contract.Error
		if !errors.As(err, &ce) || *ce != want {
			t.Errorf("%s: err = %v, want %v", file, err, want)
		}
	}
}

func TestEventActionBadTokenIsGrammarAtToken(t *testing.T) {
	if len(readFixture(t, "events/action-bad-token.json")) == 0 {
		t.Fatal("fixture missing")
	}
	ce := eventErr(t, "events/action-bad-token.json")
	if ce == nil || ce.Code != contract.CodeGrammar || ce.At != "/action/token" {
		t.Fatalf("got %v", ce)
	}
}

func TestEventActionWithMessageIsForbidden(t *testing.T) {
	ce := eventErr(t, "events/action-with-message.json")
	if ce == nil || ce.Code != contract.CodeForbidden || ce.At != "/message" {
		t.Fatalf("got %v", ce)
	}
}

func TestEventActionMissingMessageRefIsDecode(t *testing.T) {
	if ce := eventErr(t, "events/action-missing-message-ref.json"); ce == nil || ce.Code != contract.CodeDecode {
		t.Fatalf("got %v", ce)
	}
}

func TestEventActionTypeWithoutPayloadIsRequiredIf(t *testing.T) {
	body := []byte(`{"event_id":"e","conversation":{"ref":"1","kind":"private"},"sender":{"ref":"1"},"type":"action"}`)
	_, err := contract.DecodeStrict[contract.Event](body)
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeRequiredIf || ce.At != "/action" {
		t.Fatalf("got %v", err)
	}
}

func TestEventMessageWithActionIsForbidden(t *testing.T) {
	body := []byte(`{"event_id":"e","conversation":{"ref":"1","kind":"private"},"sender":{"ref":"1"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"x"}]},"action":{"ref":"1","message_ref":"2","token":"tk_1"}}`)
	_, err := contract.DecodeStrict[contract.Event](body)
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeForbidden || ce.At != "/action" {
		t.Fatalf("got %v", err)
	}
}
