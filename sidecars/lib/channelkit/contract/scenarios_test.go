package contract_test

import (
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
	if len(sc) != 7 {
		t.Fatalf("got %d scenarios, want 7", len(sc))
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
