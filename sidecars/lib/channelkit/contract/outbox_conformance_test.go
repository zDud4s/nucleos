package contract_test

import (
	"errors"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("outbox_response", func(b []byte) (any, error) {
		v, err := contract.DecodeOutboxResponse(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

func TestOutboxMixedLaneSequence(t *testing.T) {
	r, err := contract.DecodeOutboxResponse(readFixture(t, "outbox/response-mixed.json"))
	if err != nil {
		t.Fatal(err)
	}
	want := []contract.Lane{contract.Ordered, contract.Immediate}
	if len(r.Actions) != len(want) {
		t.Fatalf("actions = %d", len(r.Actions))
	}
	for i, raw := range r.Actions {
		a, err := contract.ParseAction(raw)
		if err != nil {
			t.Fatalf("action %d: %v", i, err)
		}
		if a.Lane() != want[i] {
			t.Errorf("action %d lane = %v, want %v", i, a.Lane(), want[i])
		}
	}
	if len(r.Ephemeral) != 1 || r.Ephemeral[0].Type != contract.EphemeralTyping || r.Ephemeral[0].Conversation != "4242" {
		t.Errorf("ephemeral = %+v", r.Ephemeral)
	}
}

func TestOutboxUnreadableActionDoesNotSpoilTheBatch(t *testing.T) {
	r, err := contract.DecodeOutboxResponse(readFixture(t, "outbox/response-mixed.json"))
	if err != nil {
		t.Fatal(err)
	}
	r.Actions[0] = readFixture(t, "outbox/unknown-action-type.json")
	_, err = contract.ParseAction(r.Actions[0])
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeDecode {
		t.Fatalf("first action: %v", err)
	}
	a, err := contract.ParseAction(r.Actions[1])
	if err != nil {
		t.Fatalf("second action: %v", err)
	}
	if a.Lane() != contract.Immediate {
		t.Errorf("lane = %v", a.Lane())
	}
}
