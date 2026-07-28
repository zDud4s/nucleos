package shortcuts

import "strings"

type Kind int

const (
	SendToAgent Kind = iota
	Kill
	Cancel
	Budget
	Proposals
	Proj
	// Mail triages whatever is waiting, now. Collecting mail is free and happens on its own;
	// classifying it costs a run, so it waits to be asked for — this is the asking.
	Mail
	// Inbox shows what the pillar knows without spending anything.
	Inbox
	Help
	// Refused is a command that was recognised but must not run from where it came — today, one
	// spoken into a voice note.
	Refused
)

type Intent struct {
	Kind Kind
	Text string
	On   bool
	Arg  string
}

// Route classifies typed input. Commands match case-insensitively: a phone that auto-capitalises
// would otherwise turn `/kill on` into free text for the orchestrator to interpret, and the one
// message that must never be interpreted is the emergency stop.
func Route(text string) Intent {
	trimmed := strings.TrimSpace(text)
	lowered := strings.ToLower(trimmed)

	switch lowered {
	case "/start", "/help":
		return Intent{Kind: Help}
	case "/kill on":
		return Intent{Kind: Kill, On: true}
	case "/kill off":
		return Intent{Kind: Kill, On: false}
	case "/cancel":
		return Intent{Kind: Cancel}
	case "/budget":
		return Intent{Kind: Budget}
	case "/proposals":
		return Intent{Kind: Proposals}
	case "/proj", "/projects":
		return Intent{Kind: Proj}
	case "/mail":
		return Intent{Kind: Mail}
	case "/inbox":
		return Intent{Kind: Inbox}
	}

	// Prefix checks run against `trimmed` rather than `lowered`: lowercasing can change a string's
	// byte length, and the argument is sliced out by index.
	if hasPrefixFold(trimmed, "/kill") {
		return Intent{Kind: Help}
	}
	if hasPrefixFold(trimmed, "/proj ") {
		return Intent{Kind: Proj, Arg: strings.TrimSpace(trimmed[len("/proj "):])}
	}

	return Intent{Kind: SendToAgent, Text: text}
}

// RouteTranscript classifies what a speech model heard. A transcript is a guess made over a noisy
// channel with no confirmation step anywhere: letting one fire `/kill off` means an autonomous
// agent can be re-armed by a misheard word, with nothing typed and nothing to point at afterwards.
// A transcript that lands on a command is therefore refused out loud; anything else is what voice
// notes are for and goes to the orchestrator unchanged.
func RouteTranscript(text string) Intent {
	if Route(text).Kind != SendToAgent {
		return Intent{Kind: Refused, Text: text}
	}
	return Intent{Kind: SendToAgent, Text: text}
}

func hasPrefixFold(s, prefix string) bool {
	return len(s) >= len(prefix) && strings.EqualFold(s[:len(prefix)], prefix)
}
