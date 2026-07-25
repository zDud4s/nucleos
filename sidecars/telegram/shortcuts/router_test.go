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
