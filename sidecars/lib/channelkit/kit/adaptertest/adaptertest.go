// Package adaptertest holds conformance helpers for adapter authors: an adapter's test
// calls CheckCapabilities with what it declares and ExecuteValid with the adapter itself,
// and sees what the core will send it for each capability it lacks.
package adaptertest

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"reflect"
	"sort"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/kit"
)

// CheckCapabilities compares caps with the seven capability scenarios, prints the output
// the core will send (scenario and file names) and fails on any inconsistency.
func CheckCapabilities(t testing.TB, caps contract.Capabilities) {
	t.Helper()
	sc, err := fixtures.Scenarios()
	if err != nil {
		t.Fatalf("scenarios: %v", err)
	}
	for _, l := range describe(caps, sc) {
		t.Log(l)
	}
	lines, _ := noButtonsOutput(sc)
	for _, l := range lines {
		t.Log(l)
	}
	for _, p := range capabilityProblems(caps, sc) {
		t.Error(p)
	}
}

// ExecuteValid runs every valid outbox fixture action through Guard and then Execute.
// Actions the guard rejects are reported and skipped: the core degrades them before they
// reach the adapter.
func ExecuteValid(t testing.TB, ad kit.Adapter) {
	t.Helper()
	n, skipped, problems := executeValid(context.Background(), ad)
	for _, s := range skipped {
		t.Log("skipped (beyond the declared capabilities): " + s)
	}
	t.Logf("executed %d valid actions", n)
	for _, p := range problems {
		t.Error(p)
	}
}

// capabilityProblems lists what is wrong with caps and with the scenario set.
func capabilityProblems(caps contract.Capabilities, sc []fixtures.Scenario) []string {
	var out []string
	if caps.MaxText == nil || *caps.MaxText <= 0 {
		out = append(out, "max_text must be a positive number")
	}
	if !has(caps.MediaOut, contract.MediaImage) && (has(caps.MediaOut, contract.MediaFile) || has(caps.MediaOut, contract.MediaAudio)) {
		out = append(out, "media_out declares file or audio without image: the no-media-out scenario keeps image and assumes file and audio absent")
	}
	out = append(out, scenarioProblems(sc)...)
	out = append(out, guardProblems(caps, sc)...)
	_, p := noButtonsOutput(sc)
	return append(out, p...)
}

// describe says, per scenario, whether it applies to caps and where its output is.
func describe(caps contract.Capabilities, sc []fixtures.Scenario) []string {
	var out []string
	for _, s := range sc {
		switch {
		case s.Missing == nil:
			out = append(out, fmt.Sprintf("scenario %s (%s): control, every capability present", s.Name, s.File))
		case lacks(caps, *s.Missing):
			out = append(out, fmt.Sprintf("scenario %s (%s): applies, the adapter lacks %s and the core degrades for it", s.Name, s.File, *s.Missing))
		default:
			out = append(out, fmt.Sprintf("scenario %s (%s): not applicable, the adapter declares %s", s.Name, s.File, *s.Missing))
		}
	}
	return out
}

func lacks(c contract.Capabilities, capability string) bool {
	switch capability {
	case "buttons":
		return !c.Buttons
	case "edit":
		return !c.Edit
	case "typing":
		return !c.Typing
	case "threads":
		return !c.Threads
	case "reply_to":
		return !c.ReplyTo
	case "media_out":
		return !has(c.MediaOut, contract.MediaFile) || !has(c.MediaOut, contract.MediaAudio)
	}
	return false
}

func has(kinds []contract.MediaKind, k contract.MediaKind) bool {
	for _, x := range kinds {
		if x == k {
			return true
		}
	}
	return false
}

func helloCaps(s fixtures.Scenario) (contract.Capabilities, error) {
	h, err := contract.DecodeStrict[contract.Hello](s.Hello)
	if err != nil {
		return contract.Capabilities{}, err
	}
	return *h.Capabilities, nil
}

// diff names the capabilities in which a and b differ.
func diff(a, b contract.Capabilities) []string {
	var out []string
	add := func(name string, eq bool) {
		if !eq {
			out = append(out, name)
		}
	}
	add("buttons", a.Buttons == b.Buttons)
	add("edit", a.Edit == b.Edit)
	add("typing", a.Typing == b.Typing)
	add("threads", a.Threads == b.Threads)
	add("reply_to", a.ReplyTo == b.ReplyTo)
	add("max_text", reflect.DeepEqual(a.MaxText, b.MaxText))
	add("media_in", sameKinds(a.MediaIn, b.MediaIn))
	add("media_out", sameKinds(a.MediaOut, b.MediaOut))
	return out
}

func sameKinds(a, b []contract.MediaKind) bool {
	x, y := append([]contract.MediaKind(nil), a...), append([]contract.MediaKind(nil), b...)
	sort.Slice(x, func(i, j int) bool { return x[i] < x[j] })
	sort.Slice(y, func(i, j int) bool { return y[i] < y[j] })
	return len(x) == len(y) && (len(x) == 0 || reflect.DeepEqual(x, y))
}

// scenarioProblems checks the fixtures themselves: full is the control and every no-X
// differs from it in exactly X.
func scenarioProblems(sc []fixtures.Scenario) []string {
	var full *fixtures.Scenario
	for i := range sc {
		if sc[i].Name == "full" {
			full = &sc[i]
		}
	}
	if full == nil || full.Missing != nil {
		return []string{"scenario full (the control) is missing"}
	}
	fc, err := helloCaps(*full)
	if err != nil {
		return []string{"scenario full: " + err.Error()}
	}
	var out []string
	for _, s := range sc {
		if s.Missing == nil {
			continue
		}
		c, err := helloCaps(s)
		if err != nil {
			out = append(out, fmt.Sprintf("scenario %s: %v", s.Name, err))
			continue
		}
		if d := diff(fc, c); len(d) != 1 || d[0] != *s.Missing {
			out = append(out, fmt.Sprintf("scenario %s must differ from full exactly in %s, differs in %v", s.Name, *s.Missing, d))
		}
	}
	return out
}

// fullSendParts reads the parts of the full scenario's send_parts stimulus.
func fullSendParts(sc []fixtures.Scenario) []contract.OutPart {
	for _, s := range sc {
		if s.Name != "full" {
			continue
		}
		for _, st := range s.Steps {
			var core struct {
				Do    string             `json:"do"`
				Parts []contract.OutPart `json:"parts"`
			}
			if json.Unmarshal(st.Core, &core) == nil && core.Do == "send_parts" {
				return core.Parts
			}
		}
	}
	return nil
}

// guardProblems confirms that, for every capability caps declares, the full scenario's
// action using it passes Guard.
func guardProblems(caps contract.Capabilities, sc []fixtures.Scenario) []string {
	var out []string
	check := func(capability string, a contract.Action) {
		if ack := kit.Guard(caps, a); ack != nil {
			out = append(out, fmt.Sprintf("%s is declared but the full action using it is rejected: %s", capability, ack.Failed.Reason))
		}
	}
	text := []contract.OutPart{{Type: contract.OutText, Text: "x"}}
	if caps.Buttons {
		check("buttons", contract.Send{ID: 1, Conversation: "c", Parts: text, Buttons: [][]contract.Button{{{Label: "Approve", Token: "tk_1"}}}})
	}
	if caps.ReplyTo {
		ref := uint64(1)
		check("reply_to", contract.Send{ID: 1, Conversation: "c", Parts: text, ReplyTo: &ref, ReplyToPlatformRef: "1"})
	}
	if caps.Edit {
		check("edit", contract.Edit{ID: 1, Conversation: "c", Target: 1, TargetPlatformRefs: []string{"1"}, Parts: text})
	}
	for _, kind := range caps.MediaOut {
		for _, p := range fullSendParts(sc) {
			if string(p.Type) == string(kind) {
				check("media_out."+string(kind), contract.Send{ID: 1, Conversation: "c", Parts: []contract.OutPart{p}})
			}
		}
	}
	return out
}

// noButtonsOutput returns the visible text the no-buttons scenario expects (read as
// English, Q3=C) and a problem for every Portuguese word found in it.
func noButtonsOutput(sc []fixtures.Scenario) ([]string, []string) {
	for _, s := range sc {
		if s.Name != "no-buttons" {
			continue
		}
		var lines, problems []string
		for _, st := range s.Steps {
			var e struct {
				Actions []struct {
					Parts []struct {
						Contains []string `json:"$contains"`
					} `json:"parts"`
				} `json:"actions"`
			}
			if json.Unmarshal(st.Expect, &e) != nil {
				continue
			}
			for _, a := range e.Actions {
				for _, p := range a.Parts {
					for _, c := range p.Contains {
						lines = append(lines, "no-buttons (capabilities/no-buttons.json) text contains: "+c)
					}
				}
			}
			low := strings.ToLower(string(st.Expect))
			for _, w := range []string{"aprovar", "rejeitar"} {
				if strings.Contains(low, w) {
					problems = append(problems, fmt.Sprintf("no-buttons text must be English (approve N / reject N), found %q", w))
				}
			}
		}
		return lines, problems
	}
	return nil, []string{"scenario no-buttons is missing"}
}

// executeValid runs each valid outbox action through Guard and Execute.
func executeValid(ctx context.Context, ad kit.Adapter) (executed int, skipped, problems []string) {
	idx, err := fixtures.Index()
	if err != nil {
		return 0, nil, []string{"index: " + err.Error()}
	}
	for _, e := range idx.Fixtures {
		if e.Shape != "action" || !strings.HasPrefix(e.File, "outbox/") {
			continue
		}
		valid := e.Valid
		if e.Go != nil && e.Go.Valid != nil {
			valid = *e.Go.Valid
		}
		if !valid {
			continue
		}
		b, err := fs.ReadFile(fixtures.FS(), e.File)
		if err != nil {
			problems = append(problems, fmt.Sprintf("%s: %v", e.File, err))
			continue
		}
		a, err := contract.ParseAction(b)
		if err != nil {
			problems = append(problems, fmt.Sprintf("%s: valid fixture does not parse: %v", e.File, err))
			continue
		}
		if ack := kit.Guard(ad.Capabilities(), a); ack != nil {
			skipped = append(skipped, fmt.Sprintf("%s (%s)", e.File, ack.Failed.Reason))
			continue
		}
		if err := executeOne(ctx, ad, a); err != nil {
			problems = append(problems, fmt.Sprintf("%s: Execute: %v", e.File, err))
			continue
		}
		executed++
	}
	return executed, skipped, problems
}

// executeOne executes a and returns its error, except for the one refusal the contract
// allows: a permanent edit_unsupported_shape on an edit whose number of parts differs from
// its number of target_platform_refs (spec section 6.2, decision Q8: an adapter edits only
// when the shapes match and otherwise refuses, and the core re-sends as a new send). The
// kit cannot render, so parts stand for pieces; an adapter that refuses an edit whose
// shapes match still fails.
func executeOne(ctx context.Context, ad kit.Adapter, a contract.Action) error {
	_, err := ad.Execute(ctx, a)
	if err == nil {
		return nil
	}
	e, ok := a.(contract.Edit)
	var ee *kit.ExecError
	if ok && errors.As(err, &ee) && !ee.Retryable && ee.Reason == "edit_unsupported_shape" &&
		len(e.Parts) != len(e.TargetPlatformRefs) {
		return nil
	}
	return err
}
