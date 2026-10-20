package adaptertest

import (
	"context"
	"errors"
	"io/fs"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

// telegramCaps reads the capabilities of hello/telegram-full.json.
func telegramCaps(t *testing.T) contract.Capabilities {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "hello/telegram-full.json")
	if err != nil {
		t.Fatal(err)
	}
	h, err := contract.DecodeStrict[contract.Hello](b)
	if err != nil {
		t.Fatal(err)
	}
	return *h.Capabilities
}

func intp(n int) *int { return &n }

type fakeAdapter struct {
	caps     contract.Capabilities
	executed []contract.Action
	err      error
}

func (f *fakeAdapter) Capabilities() contract.Capabilities  { return f.caps }
func (f *fakeAdapter) Typing(context.Context, string) error { return nil }
func (f *fakeAdapter) Execute(_ context.Context, a contract.Action) ([]string, error) {
	f.executed = append(f.executed, a)
	if f.err != nil {
		return nil, f.err
	}
	return []string{"p1"}, nil
}

func realScenarios(t *testing.T) []fixtures.Scenario {
	t.Helper()
	sc, err := fixtures.Scenarios()
	if err != nil {
		t.Fatal(err)
	}
	return sc
}

func TestAdapterTestCheckCapabilitiesPassesForTelegramFull(t *testing.T) {
	caps := telegramCaps(t)
	if p := capabilityProblems(caps, realScenarios(t)); len(p) != 0 {
		t.Fatalf("problems = %v, want none", p)
	}
	CheckCapabilities(t, caps) // must not fail t, and prints the core's output
}

func TestAdapterTestCheckCapabilitiesFailsOnZeroOrMissingMaxText(t *testing.T) {
	for name, mt := range map[string]*int{"zero": intp(0), "nil": nil, "negative": intp(-3)} {
		t.Run(name, func(t *testing.T) {
			caps := telegramCaps(t)
			caps.MaxText = mt
			p := capabilityProblems(caps, realScenarios(t))
			if len(p) == 0 || !strings.Contains(strings.Join(p, "\n"), "max_text") {
				t.Fatalf("problems = %v, want a max_text problem", p)
			}
		})
	}
}

func TestAdapterTestCheckCapabilitiesFailsOnMediaOutTheNoMediaOutScenarioAssumesAbsent(t *testing.T) {
	caps := telegramCaps(t)
	caps.MediaOut = []contract.MediaKind{contract.MediaFile, contract.MediaAudio} // no image anchor
	p := strings.Join(capabilityProblems(caps, realScenarios(t)), "\n")
	if !strings.Contains(p, "no-media-out") {
		t.Fatalf("problems = %q, want one naming no-media-out", p)
	}
}

func TestAdapterTestCheckCapabilitiesAcceptsNoMediaOutAtAll(t *testing.T) {
	caps := telegramCaps(t)
	caps.MediaOut = nil
	if p := capabilityProblems(caps, realScenarios(t)); len(p) != 0 {
		t.Fatalf("problems = %v, want none", p)
	}
}

func TestAdapterTestScenariosDifferFromFullInExactlyTheirMissingCapability(t *testing.T) {
	sc := realScenarios(t)
	if p := scenarioProblems(sc); len(p) != 0 {
		t.Fatalf("real fixtures: %v", p)
	}
	// Break no-edit so it also loses buttons.
	var bad []fixtures.Scenario
	for _, s := range sc {
		if s.Name == "no-edit" {
			s.Hello = []byte(strings.Replace(string(s.Hello), `"typing": true`, `"typing": false`, 1))
		}
		bad = append(bad, s)
	}
	p := strings.Join(scenarioProblems(bad), "\n")
	if !strings.Contains(p, "no-edit") || !strings.Contains(p, "typing") {
		t.Fatalf("problems = %q, want no-edit to differ in typing", p)
	}
}

func TestAdapterTestFullScenarioIsRequired(t *testing.T) {
	var none []fixtures.Scenario
	for _, s := range realScenarios(t) {
		if s.Name != "full" {
			none = append(none, s)
		}
	}
	if len(scenarioProblems(none)) == 0 {
		t.Fatal("a missing full scenario must be a problem")
	}
}

func TestAdapterTestNoButtonsIsReadAsEnglish(t *testing.T) {
	sc := realScenarios(t)
	lines, p := noButtonsOutput(sc)
	if len(p) != 0 {
		t.Fatalf("problems = %v", p)
	}
	if got := strings.Join(lines, "\n"); !strings.Contains(got, "approve 12") || !strings.Contains(got, "reject 12") {
		t.Fatalf("output = %q, want approve 12 / reject 12", got)
	}
	var bad []fixtures.Scenario
	for _, s := range sc {
		if s.Name == "no-buttons" {
			s.Steps = append([]fixtures.ScenarioStep(nil), s.Steps...)
			s.Steps[0].Expect = []byte(strings.ReplaceAll(string(s.Steps[0].Expect), "approve 12", "aprovar 12"))
		}
		bad = append(bad, s)
	}
	if _, p := noButtonsOutput(bad); len(p) == 0 {
		t.Fatal("aprovar must be a problem")
	}
}

func TestAdapterTestExecuteValidRunsEveryValidActionThroughGuardAndExecute(t *testing.T) {
	ad := &fakeAdapter{caps: telegramCaps(t)}
	n, skipped, problems := executeValid(context.Background(), ad)
	if len(problems) != 0 {
		t.Fatalf("problems = %v", problems)
	}
	if n == 0 || n != len(ad.executed) || len(skipped) != 0 {
		t.Fatalf("executed %d (recorded %d), skipped %v", n, len(ad.executed), skipped)
	}
	seen := map[contract.ActionType]bool{}
	for _, a := range ad.executed {
		seen[a.Type()] = true
	}
	if !seen[contract.ActionSend] || !seen[contract.ActionEdit] {
		t.Fatalf("types seen = %v, want send and edit", seen)
	}
	ExecuteValid(t, ad)
}

func TestAdapterTestExecuteValidSkipsWhatGuardRejects(t *testing.T) {
	caps := telegramCaps(t)
	caps.Buttons, caps.ReplyTo = false, false
	ad := &fakeAdapter{caps: caps}
	_, skipped, problems := executeValid(context.Background(), ad)
	if len(problems) != 0 {
		t.Fatalf("problems = %v", problems)
	}
	found := false
	for _, s := range skipped {
		found = found || strings.Contains(s, "send-full.json")
	}
	if !found {
		t.Fatalf("skipped = %v, want send-full.json", skipped)
	}
	for _, a := range ad.executed {
		if s, ok := a.(contract.Send); ok && len(s.Buttons) > 0 {
			t.Fatal("a send with buttons reached Execute")
		}
	}
}

func TestAdapterTestExecuteValidReportsExecuteErrors(t *testing.T) {
	ad := &fakeAdapter{caps: telegramCaps(t), err: errors.New("boom")}
	_, _, problems := executeValid(context.Background(), ad)
	if len(problems) == 0 || !strings.Contains(strings.Join(problems, "\n"), "boom") {
		t.Fatalf("problems = %v, want the Execute error", problems)
	}
}
