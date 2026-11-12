package daemon

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

// captureDeliver runs one Deliver against a stub núcleo and returns the exact bytes it received.
// The bytes are the point: every earlier test on either side of this boundary asserted on decoded
// structs, where a nil slice and an empty one stop being distinguishable.
func captureDeliver(t *testing.T, batch Batch) map[string]any {
	t.Helper()
	var raw []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("reading the request body: %v", err)
		}
		raw = body
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ingested":0,"duplicates":0,"cursor":10}`))
	}))
	defer server.Close()

	if _, err := New(server.URL, "test-token").Deliver(batch); err != nil {
		t.Fatalf("Deliver: %v", err)
	}

	var decoded map[string]any
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatalf("the delivered payload is not JSON: %v", err)
	}
	return decoded
}

// The ordinary poll — mail read, nothing skipped — used to marshal Skipped as `null` and come back
// 422 from a núcleo that asked for a list. It replayed every five minutes and no mail ever landed.
func TestDeliverSendsListsNotNull(t *testing.T) {
	payload := captureDeliver(t, Batch{
		Mailbox:        "INBOX",
		UIDValidity:    1,
		MaxUIDExamined: 10,
		Messages: []Message{{
			UID:        10,
			FromAddr:   "ana@company.com",
			ReceivedAt: "2026-07-28T11:00:00+00:00",
		}},
	})

	for _, field := range []string{"skipped", "messages"} {
		value, present := payload[field]
		if !present {
			t.Errorf("%q is missing from the payload", field)
			continue
		}
		if _, isList := value.([]any); !isList {
			t.Errorf("%q was sent as %#v, want a list", field, value)
		}
	}
}

// A pass that examined uids and delivered nothing still has to land, or the cursor never moves past
// mail the sidecar already decided about.
func TestDeliverSendsEmptyListsWhenNothingWasRead(t *testing.T) {
	payload := captureDeliver(t, Batch{Mailbox: "INBOX", UIDValidity: 1, MaxUIDExamined: 10})

	for _, field := range []string{"skipped", "messages"} {
		list, isList := payload[field].([]any)
		if !isList {
			t.Errorf("%q was sent as %#v, want a list", field, payload[field])
			continue
		}
		if len(list) != 0 {
			t.Errorf("%q has %d entries, want none", field, len(list))
		}
	}
}
