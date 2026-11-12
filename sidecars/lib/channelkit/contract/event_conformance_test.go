package contract_test

import (
	"encoding/json"
	"errors"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("event", func(b []byte) (any, error) {
		v, err := contract.DecodeStrict[contract.Event](b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

// eventErr decodes a fixture strictly and returns the contract error, if any.
func eventErr(t *testing.T, file string) *contract.Error {
	t.Helper()
	_, err := contract.DecodeStrict[contract.Event](readFixture(t, file))
	if err == nil {
		return nil
	}
	var ce *contract.Error
	if !errors.As(err, &ce) {
		t.Fatalf("%s: not a contract error: %v", file, err)
	}
	return ce
}

func TestEventMessageFullHasFiveParts(t *testing.T) {
	e, err := contract.DecodeStrict[contract.Event](readFixture(t, "events/message-full.json"))
	if err != nil {
		t.Fatal(err)
	}
	if e.Message == nil || len(e.Message.Parts) != 5 {
		t.Fatalf("want 5 parts: %+v", e.Message)
	}
	want := []contract.PartType{contract.PartText, contract.PartAudio, contract.PartImage, contract.PartFile, contract.PartUnsupported}
	for i, p := range e.Message.Parts {
		if p.Type == nil || *p.Type != want[i] {
			t.Errorf("part %d type = %v, want %s", i, p.Type, want[i])
		}
	}
	if d := e.Message.Parts[1].DurationMS; d == nil || *d != 4100 {
		t.Errorf("duration_ms = %v", d)
	}
}

func TestEventAtKeptAsString(t *testing.T) {
	e, err := contract.DecodeStrict[contract.Event](readFixture(t, "events/message-group-image.json"))
	if err != nil {
		t.Fatal(err)
	}
	if e.At == nil || *e.At != "2026-10-19T10:31:00+01:00" {
		t.Fatalf("at = %v", e.At)
	}
	out, _ := json.Marshal(e)
	var m map[string]any
	if err := json.Unmarshal(out, &m); err != nil || m["at"] != "2026-10-19T10:31:00+01:00" {
		t.Errorf("at must round-trip exactly: %s", out)
	}
}

func TestEventAbsentAtStaysAbsent(t *testing.T) {
	e, err := contract.DecodeStrict[contract.Event](readFixture(t, "events/message-private-text.json"))
	if err != nil {
		t.Fatal(err)
	}
	out, _ := json.Marshal(e)
	var m map[string]any
	_ = json.Unmarshal(out, &m)
	if _, ok := m["at"]; ok || e.At != nil {
		t.Errorf("at must stay absent: %s", out)
	}
}

func TestEventNegativeDurationIsDecodeNotRange(t *testing.T) {
	if ce := eventErr(t, "events/negative-duration.json"); ce == nil || ce.Code != contract.CodeDecode {
		t.Fatalf("got %v, want decode", ce)
	}
}

func TestEventOriginInBodyIsDecode(t *testing.T) {
	if ce := eventErr(t, "events/origin-in-body.json"); ce == nil || ce.Code != contract.CodeDecode {
		t.Fatalf("got %v, want decode", ce)
	}
}

func TestEventUnknownTypeIsOneOf(t *testing.T) {
	body := []byte(`{"event_id":"e","conversation":{"ref":"1","kind":"private"},"sender":{"ref":"1"},"type":"reaction"}`)
	_, err := contract.DecodeStrict[contract.Event](body)
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeOneOf || ce.At != "/type" {
		t.Fatalf("got %v, want one_of /type", err)
	}
}

func TestEventRefLimits(t *testing.T) {
	long := make([]byte, 257)
	for i := range long {
		long[i] = 'a'
	}
	body := []byte(`{"event_id":"` + string(long) + `","conversation":{"ref":"1","kind":"private"},"sender":{"ref":"1"},"type":"message","message":{"ref":"1","parts":[{"type":"text","text":"x"}]}}`)
	_, err := contract.DecodeStrict[contract.Event](body)
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeGrammar || ce.At != "/event_id" {
		t.Fatalf("got %v, want grammar /event_id", err)
	}
}
