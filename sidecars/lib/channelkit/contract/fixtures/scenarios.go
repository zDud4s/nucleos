package fixtures

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io/fs"
	"path"
	"strings"
)

// Scenario is one capabilities/*.json file: an adapter that lacks exactly one capability
// (Missing), a setup and ordered steps. Every payload stays raw JSON so this package
// never imports package contract (the contract tests import this one).
type Scenario struct {
	// Name is the file name without directory and extension, e.g. "no-buttons".
	Name string
	// File is the slash path inside the fixture tree.
	File string
	// Missing names the absent capability; nil for the full control scenario.
	Missing *string
	Hello   json.RawMessage
	Setup   json.RawMessage
	// Forbid is nil when the scenario declares none.
	Forbid json.RawMessage
	Steps  []ScenarioStep
}

// ScenarioStep is one step: exactly one of Core (a stimulus) or Event (a valid event
// posted to /events) is set; Expect is nil when the step asserts nothing.
type ScenarioStep struct {
	Core   json.RawMessage
	Event  json.RawMessage
	Expect json.RawMessage
}

type scenarioFile struct {
	Missing *string         `json:"missing"`
	Hello   json.RawMessage `json:"hello"`
	Setup   json.RawMessage `json:"setup"`
	Forbid  json.RawMessage `json:"forbid"`
	Steps   []struct {
		Core   json.RawMessage `json:"core"`
		Event  json.RawMessage `json:"event"`
		Expect json.RawMessage `json:"expect"`
	} `json:"steps"`
}

// Scenarios returns every index entry whose shape is "scenario", in index order.
func Scenarios() ([]Scenario, error) {
	idx, err := Index()
	if err != nil {
		return nil, err
	}
	var out []Scenario
	for _, e := range idx.Fixtures {
		if e.Shape != "scenario" {
			continue
		}
		b, err := fs.ReadFile(files, e.File)
		if err != nil {
			return nil, err
		}
		var f scenarioFile
		dec := json.NewDecoder(bytes.NewReader(b))
		dec.DisallowUnknownFields()
		if err := dec.Decode(&f); err != nil {
			return nil, fmt.Errorf("%s: %w", e.File, err)
		}
		s := Scenario{
			Name:    strings.TrimSuffix(path.Base(e.File), ".json"),
			File:    e.File,
			Missing: f.Missing,
			Hello:   f.Hello,
			Setup:   f.Setup,
			Forbid:  f.Forbid,
		}
		for _, st := range f.Steps {
			s.Steps = append(s.Steps, ScenarioStep{Core: st.Core, Event: st.Event, Expect: st.Expect})
		}
		out = append(out, s)
	}
	return out, nil
}
