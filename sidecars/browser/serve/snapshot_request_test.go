package serve

import (
	"encoding/json"
	"net/http"
	"testing"

	"nucleosbrowser/browser"
)

// TestEveryCursorOnTheWireReachesTheDriver.
//
// The failure this exists for is the quietest one in the whole pillar, because every part of it
// works. The driver bounds a snapshot and offers `controls_next`; the núcleo hands it back as
// `controls_from`; the JSON is well-formed; the answer is a valid snapshot. And the field had no
// home in this package's request struct, so `encoding/json` dropped it and the reading restarted at
// the first control — an agent following the offer got page one, again, with nothing anywhere
// reporting a problem.
//
// A test per field rather than one for the shape, because a missing field is exactly what a
// round-trip of the shape does NOT catch: what is not there marshals and unmarshals perfectly.
func TestEveryCursorOnTheWireReachesTheDriver(t *testing.T) {
	for _, one := range []struct {
		name string
		body map[string]any
		want browser.SnapshotRequest
	}{
		{
			name: "changes only",
			body: map[string]any{"session_id": "s1", "changes_only": true},
			want: browser.SnapshotRequest{ChangesOnly: true},
		},
		{
			name: "text cursor",
			body: map[string]any{"session_id": "s1", "text_from": 4096},
			want: browser.SnapshotRequest{TextFrom: 4096},
		},
		{
			name: "controls cursor",
			body: map[string]any{"session_id": "s1", "controls_from": 300},
			want: browser.SnapshotRequest{ControlsFrom: 300},
		},
		{
			name: "find",
			body: map[string]any{"session_id": "s1", "find": "invoices"},
			want: browser.SnapshotRequest{Find: "invoices"},
		},
	} {
		t.Run(one.name, func(t *testing.T) {
			driver := &browser.Fake{FenceAttached: true}
			server := testServer(t, driver)

			opened := post(t, server, "/open", opening("https://example.org/"), true)
			var session browser.Session
			if err := json.NewDecoder(opened.Body).Decode(&session); err != nil {
				t.Fatalf("open: %v", err)
			}
			opened.Body.Close()
			one.body["session_id"] = string(session.ID)

			response := post(t, server, "/snapshot", one.body, true)
			response.Body.Close()
			if response.StatusCode != http.StatusOK {
				t.Fatalf("snapshot: %d", response.StatusCode)
			}

			if len(driver.Asked) != 1 {
				t.Fatalf("the driver was asked %d times", len(driver.Asked))
			}
			if got := driver.Asked[0]; got != one.want {
				t.Errorf("the driver was asked for %+v, and the wire said %+v", got, one.want)
			}
		})
	}
}
