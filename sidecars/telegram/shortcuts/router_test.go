package shortcuts

import "testing"

func TestRoute(t *testing.T) {
	tests := []struct {
		name string
		text string
		want Intent
	}{
		{name: "start", text: "/start", want: Intent{Kind: Help}},
		{name: "help with whitespace", text: "  /help  ", want: Intent{Kind: Help}},
		{name: "kill on", text: "/kill on", want: Intent{Kind: Kill, On: true}},
		{name: "kill off", text: "/kill off", want: Intent{Kind: Kill, On: false}},
		{name: "bare kill", text: "/kill", want: Intent{Kind: Help}},
		{name: "invalid kill", text: "/kill maybe", want: Intent{Kind: Help}},
		{name: "cancel", text: "/cancel", want: Intent{Kind: Cancel}},
		{name: "budget", text: "/budget", want: Intent{Kind: Budget}},
		{name: "proposals", text: "/proposals", want: Intent{Kind: Proposals}},
		{name: "proj empty", text: "/proj", want: Intent{Kind: Proj}},
		{name: "projects", text: "/projects", want: Intent{Kind: Proj}},
		{name: "proj name", text: "/proj nucleos", want: Intent{Kind: Proj, Arg: "nucleos"}},
		{name: "unknown command", text: "/unknown", want: Intent{Kind: SendToAgent, Text: "/unknown"}},
		{name: "free text", text: "  ask the agent  ", want: Intent{Kind: SendToAgent, Text: "  ask the agent  "}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := Route(tt.text); got != tt.want {
				t.Errorf("Route(%q) = %+v, want %+v", tt.text, got, tt.want)
			}
		})
	}
}

// A phone that auto-capitalises turns the emergency stop into free text for the orchestrator to
// interpret. `/KILL on` has to be the kill switch, not a prompt.
func TestCommandsAreRecognisedWhateverTheirCase(t *testing.T) {
	tests := []struct {
		text string
		want Intent
	}{
		{text: "/KILL on", want: Intent{Kind: Kill, On: true}},
		{text: "/Kill Off", want: Intent{Kind: Kill, On: false}},
		{text: "/HELP", want: Intent{Kind: Help}},
		{text: "/Cancel", want: Intent{Kind: Cancel}},
		{text: "/Proposals", want: Intent{Kind: Proposals}},
		{text: "/INBOX", want: Intent{Kind: Inbox}},
		{text: "/Mail", want: Intent{Kind: Mail}},
		{text: "/Budget", want: Intent{Kind: Budget}},
		{text: "/PROJECTS", want: Intent{Kind: Proj}},
		// The command matches case-insensitively; its argument is data and keeps its own case.
		{text: "/Proj NucleOS", want: Intent{Kind: Proj, Arg: "NucleOS"}},
	}

	for _, tt := range tests {
		t.Run(tt.text, func(t *testing.T) {
			if got := Route(tt.text); got != tt.want {
				t.Errorf("Route(%q) = %+v, want %+v", tt.text, got, tt.want)
			}
		})
	}
}

// A transcript is a speech model's guess over a noisy channel. Letting it fire a command means an
// autonomous agent can be re-armed by a misheard word with nothing typed anywhere, so a transcript
// that looks like a command is refused out loud instead of executed quietly. Everything else is
// what a voice note is actually for, and still reaches the orchestrator.
func TestAVoiceTranscriptCannotFireADeterministicCommand(t *testing.T) {
	if got := RouteTranscript("/kill off"); got.Kind != Refused {
		t.Errorf("RouteTranscript(%q) = %+v, want it refused", "/kill off", got)
	}
	if got := RouteTranscript("/KILL OFF"); got.Kind != Refused {
		t.Errorf("RouteTranscript(%q) = %+v, want it refused", "/KILL OFF", got)
	}
	if got := RouteTranscript("cancel my last turn please"); got.Kind != SendToAgent {
		t.Errorf("RouteTranscript(prose) = %+v, want it sent to the orchestrator", got)
	}
}

func TestRouteErrandCommands(t *testing.T) {
	for _, tc := range []struct {
		text string
		want Intent
	}{
		{"/assunto carros usados", Intent{Kind: OpenErrand, Arg: "carros usados"}},
		// Capitalised by a phone's keyboard, and the argument keeps its own case: the name is what
		// the folder is minted from and what the person will read back in `/assuntos`.
		{"/Assunto Carros", Intent{Kind: OpenErrand, Arg: "Carros"}},
		{"/assunto", Intent{Kind: OpenErrand}},
		{"/assuntos", Intent{Kind: ListErrands}},
		{"/pausa", Intent{Kind: PauseErrand}},
		{"/retomar", Intent{Kind: ResumeErrand}},
		{"/fim", Intent{Kind: CloseErrand}},
		{"/cerebro local", Intent{Kind: SetBrain, Arg: "local"}},
		{"/cerebro cloud", Intent{Kind: SetBrain, Arg: "cloud"}},
		{"/CEREBRO Cloud", Intent{Kind: SetBrain, Arg: "cloud"}},
		// A brain this daemon does not know is not silently sent on to be interpreted: `from_wire`
		// on the other side resolves anything unknown to a default, so a typo would move the errand
		// without saying so.
		{"/cerebro nuvem", Intent{Kind: SetBrain}},
	} {
		t.Run(tc.text, func(t *testing.T) {
			if got := Route(tc.text); got != tc.want {
				t.Errorf("Route(%q) = %+v, want %+v", tc.text, got, tc.want)
			}
		})
	}
}

// `/assuntos` must not be swallowed by a prefix test for `/assunto`. They are different commands and
// one of them opens something.
func TestListErrandsIsNotAPrefixOfOpenErrand(t *testing.T) {
	if got := Route("/assuntos").Kind; got != ListErrands {
		t.Errorf("Route(\"/assuntos\") = %v, want ListErrands", got)
	}
}

// A command heard in a voice note is a guess made over a noisy channel. `/fim` closes an errand and
// `/assunto` opens one; neither is a thing to do on a misheard word, with nothing typed and nothing
// to point at afterwards.
func TestATranscriptCannotOpenOrCloseAnErrand(t *testing.T) {
	for _, text := range []string{"/assunto carros", "/assuntos", "/pausa", "/retomar", "/fim", "/cerebro cloud"} {
		if got := RouteTranscript(text).Kind; got != Refused {
			t.Errorf("RouteTranscript(%q) = %v, want Refused", text, got)
		}
	}
}
