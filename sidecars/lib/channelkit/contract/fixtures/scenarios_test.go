package fixtures

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
)

func loadScenarios(t *testing.T) []Scenario {
	t.Helper()
	sc, err := Scenarios()
	if err != nil {
		t.Fatalf("Scenarios(): %v", err)
	}
	return sc
}

func TestScenariosShape(t *testing.T) {
	sc := loadScenarios(t)
	if len(sc) != 8 {
		t.Fatalf("got %d scenarios, want 8", len(sc))
	}
	for _, s := range sc {
		if len(s.Hello) == 0 || len(s.Steps) == 0 {
			t.Errorf("%s: empty hello or steps", s.Name)
		}
		for i, st := range s.Steps {
			if (len(st.Core) == 0) == (len(st.Event) == 0) {
				t.Errorf("%s step %d: needs exactly one of core and event", s.Name, i)
			}
		}
	}
}

// collectStrings walks a decoded JSON value and calls f for every string, key or value.
func collectStrings(v any, f func(string)) {
	switch x := v.(type) {
	case string:
		f(x)
	case []any:
		for _, e := range x {
			collectStrings(e, f)
		}
	case map[string]any:
		for k, e := range x {
			f(k)
			collectStrings(e, f)
		}
	}
}

func TestScenarioNoButtonsEnglish(t *testing.T) {
	forbidden := []string{"aprovar", "rejeitar", "aprovado", "rejeitado"}
	var nb *Scenario
	sc := loadScenarios(t)
	for i, s := range sc {
		if s.Name == "no-buttons" {
			nb = &sc[i]
		}
		all := [][]byte{s.Hello, s.Setup, s.Forbid}
		for _, st := range s.Steps {
			all = append(all, st.Core, st.Event, st.Expect)
		}
		for _, raw := range all {
			if len(raw) == 0 {
				continue
			}
			var v any
			if err := json.Unmarshal(raw, &v); err != nil {
				t.Fatalf("%s: %v", s.Name, err)
			}
			collectStrings(v, func(str string) {
				low := strings.ToLower(str)
				for _, w := range forbidden {
					if strings.Contains(low, w) {
						t.Errorf("%s: string %q contains %q", s.Name, str, w)
					}
				}
			})
		}
	}
	if nb == nil {
		t.Fatal("no-buttons scenario missing")
	}
	var expect, event bytes.Buffer
	for _, st := range nb.Steps {
		expect.Write(st.Expect)
		event.Write(st.Event)
	}
	for _, w := range []string{"approve 12", "reject 12"} {
		if !strings.Contains(expect.String(), w) {
			t.Errorf("no-buttons expect lacks %q", w)
		}
	}
	if !strings.Contains(event.String(), `"text": "approve 12"`) {
		t.Error("no-buttons reply event does not carry text \"approve 12\"")
	}
}
