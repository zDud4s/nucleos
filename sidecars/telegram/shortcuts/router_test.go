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

// The errand commands are gone: what used to open, list, pause, resume or close one, or switch its
// brain, is now ordinary text for the agent.
func TestFormerErrandCommandsGoToTheAgent(t *testing.T) {
	for _, text := range []string{"/assunto carros", "/assuntos", "/pausa", "/retomar", "/fim", "/cerebro local"} {
		got := Route(text)
		if got.Kind != SendToAgent || got.Text != text {
			t.Errorf("Route(%q) = %+v, want SendToAgent with the text unchanged", text, got)
		}
	}
}

func TestRouteNote(t *testing.T) {
	cases := []struct {
		in   string
		want Intent
	}{
		{"/note buy milk", Intent{Kind: Note, Arg: "buy milk"}},
		{"/NOTE  Buy Milk  ", Intent{Kind: Note, Arg: "Buy Milk"}},
		{"/Note a /kill b", Intent{Kind: Note, Arg: "a /kill b"}},
		{"/note", Intent{Kind: Note, Arg: ""}},
		{"  /Note  ", Intent{Kind: Note, Arg: ""}},
		// A phone types a long note on a new line, and a group's command menu inserts the bot's
		// name. Either one missed would send the note to the agent as a message.
		{"/note\nline one\nline two", Intent{Kind: Note, Arg: "line one\nline two"}},
		{"/note\tbuy milk", Intent{Kind: Note, Arg: "buy milk"}},
		{"/note@NucleosBot buy milk", Intent{Kind: Note, Arg: "buy milk"}},
		{"/note@NucleosBot\nbuy milk", Intent{Kind: Note, Arg: "buy milk"}},
		{"/note@NucleosBot", Intent{Kind: Note, Arg: ""}},
		{"/notes for the agent", Intent{Kind: SendToAgent, Text: "/notes for the agent"}},
	}
	for _, c := range cases {
		if got := Route(c.in); got != c.want {
			t.Errorf("Route(%q) = %+v, want %+v", c.in, got, c.want)
		}
	}
}

func TestRouteTranscriptRefusesNote(t *testing.T) {
	got := RouteTranscript("/note buy milk")
	if got.Kind != Refused {
		t.Errorf("RouteTranscript(/note ...) kind = %v, want Refused", got.Kind)
	}
}
