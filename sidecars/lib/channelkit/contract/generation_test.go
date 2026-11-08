package contract_test

import (
	"encoding/json"
	"errors"
	"testing"

	"nucleoschannelkit/contract"
)

// Contract 1.1: every lease carries a generation and every ack echoes it, so the
// core can ignore the late ack of a lease that expired and was handed out again.

func TestAckEchoesGenerationOnTheWire(t *testing.T) {
	a, err := contract.DecodeAck([]byte(`{"failed":{"retryable":true,"reason":"network"},"generation":3}`))
	if err != nil {
		t.Fatal(err)
	}
	if a.Generation != 3 {
		t.Fatalf("Generation = %d, want 3", a.Generation)
	}
	got, err := json.Marshal(a)
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"failed":{"retryable":true,"reason":"network"},"generation":3}`; string(got) != want {
		t.Fatalf("marshal = %s, want %s", got, want)
	}
}

func TestAckWithoutGenerationStaysLegacy(t *testing.T) {
	a, err := contract.DecodeAck([]byte(`{"ok":true}`))
	if err != nil {
		t.Fatal(err)
	}
	if a.Generation != 0 {
		t.Fatalf("Generation = %d, want 0", a.Generation)
	}
	got, _ := json.Marshal(a)
	if string(got) != `{"ok":true}` {
		t.Fatalf("marshal = %s, want no generation key", got)
	}
}

func TestAckGenerationZeroIsOutOfRange(t *testing.T) {
	_, err := contract.DecodeAck([]byte(`{"ok":true,"generation":0}`))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeRange || ce.At != "/generation" {
		t.Fatalf("err = %v, want range at /generation", err)
	}
}

func TestActionCarriesLeaseGeneration(t *testing.T) {
	cases := map[string]string{
		"send":          `{"id":41,"type":"send","conversation":"4242","parts":[{"type":"text","text":"Done."}],"generation":2}`,
		"edit":          `{"id":41,"type":"edit","conversation":"4242","target":40,"target_platform_refs":["9"],"parts":[{"type":"text","text":"Done."}],"generation":2}`,
		"answer_action": `{"id":41,"type":"answer_action","conversation":"4242","action_ref":"cb1","text":"ok","generation":2}`,
	}
	for name, raw := range cases {
		t.Run(name, func(t *testing.T) {
			a, err := contract.ParseAction(json.RawMessage(raw))
			if err != nil {
				t.Fatal(err)
			}
			if a.LeaseGeneration() != 2 {
				t.Fatalf("LeaseGeneration = %d, want 2", a.LeaseGeneration())
			}
			got, err := json.Marshal(a)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != raw {
				t.Fatalf("marshal = %s, want %s", got, raw)
			}
		})
	}
}

func TestActionWithoutGenerationIsZero(t *testing.T) {
	a, err := contract.ParseAction(readFixture(t, "outbox/send-text.json"))
	if err != nil {
		t.Fatal(err)
	}
	if a.LeaseGeneration() != 0 {
		t.Fatalf("LeaseGeneration = %d, want 0 (a 1.0 core)", a.LeaseGeneration())
	}
}

func TestRescueAckEchoesTheActionGeneration(t *testing.T) {
	_, ack, err := contract.DecodeAction([]byte(`{"id":41,"type":"send","conversation":"4242","parts":[],"generation":5}`))
	if err == nil || ack == nil {
		t.Fatalf("err = %v, ack = %v; want an error and a rescue ack", err, ack)
	}
	if ack.Generation != 5 {
		t.Fatalf("rescue Generation = %d, want 5", ack.Generation)
	}
}
