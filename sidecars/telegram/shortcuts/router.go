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
	Help
)

type Intent struct {
	Kind Kind
	Text string
	On   bool
	Arg  string
}

func Route(text string) Intent {
	trimmed := strings.TrimSpace(text)

	switch trimmed {
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
	}

	if strings.HasPrefix(trimmed, "/kill") {
		return Intent{Kind: Help}
	}
	if strings.HasPrefix(trimmed, "/proj ") {
		return Intent{Kind: Proj, Arg: strings.TrimSpace(strings.TrimPrefix(trimmed, "/proj "))}
	}

	return Intent{Kind: SendToAgent, Text: text}
}
