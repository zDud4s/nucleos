package contract_test

import (
	"encoding/json"
	"sort"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

func scenarios(t *testing.T) []fixtures.Scenario {
	t.Helper()
	sc, err := fixtures.Scenarios()
	if err != nil {
		t.Fatalf("Scenarios(): %v", err)
	}
	return sc
}

func TestScenariosDecode(t *testing.T) {
	sc := scenarios(t)
	if len(sc) != 8 {
		t.Fatalf("got %d scenarios, want 8", len(sc))
	}
	for _, s := range sc {
		t.Run(s.Name, func(t *testing.T) {
			h, err := contract.DecodeStrict[contract.Hello](s.Hello)
			if err != nil {
				t.Fatalf("hello: %v", err)
			}
			if h.Kind != "fake" {
				t.Errorf("hello kind = %q, want fake", h.Kind)
			}
			c := h.Capabilities
			has := func(kinds []contract.MediaKind, k contract.MediaKind) bool {
				for _, x := range kinds {
					if x == k {
						return true
					}
				}
				return false
			}
			if s.Missing != nil {
				switch *s.Missing {
				case "buttons":
					if c.Buttons {
						t.Error("buttons present")
					}
				case "edit":
					if c.Edit {
						t.Error("edit present")
					}
				case "threads":
					if c.Threads {
						t.Error("threads present")
					}
				case "typing":
					if c.Typing {
						t.Error("typing present")
					}
				case "reply_to":
					if c.ReplyTo {
						t.Error("reply_to present")
					}
				case "media_out":
					if has(c.MediaOut, "file") || has(c.MediaOut, "audio") || !has(c.MediaOut, "image") {
						t.Errorf("media_out = %v, want image without file and audio", c.MediaOut)
					}
				default:
					t.Errorf("unknown missing %q", *s.Missing)
				}
			}
			for i, st := range s.Steps {
				if len(st.Event) == 0 {
					continue
				}
				if _, err := contract.DecodeStrict[contract.Event](st.Event); err != nil {
					t.Errorf("step %d event: %v", i, err)
				}
			}
		})
	}
}

// TestConformanceProgressEditSequence pins the progress-edits scenario: an adapter that
// declares edit sees the ack as a send, then one edit of that ack per streaming tick, then the
// "finished" edit ahead of the final reply, and every edit names its target.
func TestConformanceProgressEditSequence(t *testing.T) {
	var s *fixtures.Scenario
	sc := scenarios(t)
	for i := range sc {
		if sc[i].Name == "progress-edits" {
			s = &sc[i]
		}
	}
	if s == nil {
		t.Fatal("progress-edits scenario missing")
	}
	if s.Missing != nil {
		t.Errorf("missing = %q, want the full control row", *s.Missing)
	}
	h, err := contract.DecodeStrict[contract.Hello](s.Hello)
	if err != nil {
		t.Fatalf("hello: %v", err)
	}
	if !h.Capabilities.Edit || !h.Capabilities.Typing {
		t.Errorf("capabilities = %+v, want edit and typing", h.Capabilities)
	}
	type action struct {
		Type    string   `json:"type"`
		Present []string `json:"$present"`
		Absent  []string `json:"$absent"`
	}
	var got [][]string
	for i, st := range s.Steps {
		var expect struct {
			Actions []action `json:"actions"`
		}
		if err := json.Unmarshal(st.Expect, &expect); err != nil {
			t.Fatalf("step %d expect: %v", i, err)
		}
		var types []string
		for _, a := range expect.Actions {
			types = append(types, a.Type)
			if a.Type == "edit" && !contains(a.Present, "target") {
				t.Errorf("step %d: an edit that does not name its target", i)
			}
			if a.Type == "send" && i == 0 && !contains(a.Absent, "target") {
				t.Errorf("step %d: the ack send must not carry a target", i)
			}
		}
		got = append(got, types)
	}
	want := [][]string{{"send"}, {"edit"}, {"edit"}, {"edit", "send"}}
	if len(got) != len(want) {
		t.Fatalf("action sequence = %v, want %v", got, want)
	}
	for i := range want {
		if len(got[i]) != len(want[i]) {
			t.Fatalf("action sequence = %v, want %v", got, want)
		}
		for j := range want[i] {
			if got[i][j] != want[i][j] {
				t.Fatalf("action sequence = %v, want %v", got, want)
			}
		}
	}
}

func contains(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

func TestScenariosMissingSet(t *testing.T) {
	var got []string
	for _, s := range scenarios(t) {
		if s.Missing != nil {
			got = append(got, *s.Missing)
		}
	}
	sort.Strings(got)
	want := []string{"buttons", "edit", "media_out", "reply_to", "threads", "typing"} // the six rows of spec 6.5
	if len(got) != len(want) {
		t.Fatalf("missing set = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("missing set = %v, want %v", got, want)
		}
	}
}
