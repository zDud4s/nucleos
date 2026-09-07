package notifier

import "strings"

// Rule is one selection the owner made: a family prefix or a kind literal, and whether rows
// matching it may be forwarded. The JSON tags mirror the núcleo's `notify_policy::Rule` — this is
// the wire shape of GET /notifications/policy, not a shape of ours.
type Rule struct {
	Selector string `json:"selector"`
	Enabled  bool   `json:"enabled"`
}

// Policy is the whole stored selection, split by scope the way the núcleo's table is: family
// rules match by prefix, kind rules match the literal and win over any family.
type Policy struct {
	Families []Rule `json:"families"`
	Kinds    []Rule `json:"kinds"`
}

// Allows answers whether a feed kind may be forwarded.
//
// The resolution, in order: an exact kind rule wins; otherwise the LONGEST matching family prefix
// decides; otherwise the row passes. Pure — no state, no I/O — so the whole decision is testable
// without a daemon, a bot or a clock.
//
// Three contracts hold, and each exists because the obvious implementation gets it wrong:
//
//   - The zero value passes everything. That is also what a failed read produces (see
//     notifyPolicy in the pipe), and it is the direction this mechanism must fail in: the thing
//     it guards against is noise, so its own failure must never be silence.
//   - An empty kind passes. It falls out of the resolution naturally — HasPrefix("", "job_") is
//     false, so an empty kind matches no family and reaches the default. It is written down as a
//     contract anyway, because the tempting reading is that an unreadable row is suspicious and
//     should be hushed. It is not a row to silence; it is a row that could not be read.
//   - A rule with an empty selector is ignored. Here the empty prefix WOULD match everything and
//     mute the channel entirely. The núcleo's validate already refuses one at the door, but the
//     policy arrives over HTTP from a daemon that may be a different version, and a defence
//     belongs on both sides when the cost of missing it is the channel going quiet.
func (p Policy) Allows(kind string) bool {
	for _, rule := range p.Kinds {
		if rule.Selector == "" {
			continue
		}
		if rule.Selector == kind {
			return rule.Enabled
		}
	}

	best := -1
	allowed := true
	for _, rule := range p.Families {
		if rule.Selector == "" {
			continue
		}
		if !strings.HasPrefix(kind, rule.Selector) {
			continue
		}
		// Longest prefix wins. Ties cannot happen: two family rules with the same selector are
		// the núcleo's DuplicateSelector refusal, and its unique index on (scope, selector) makes
		// the pair unstorable even if a refusal were ever missed.
		if len(rule.Selector) > best {
			best = len(rule.Selector)
			allowed = rule.Enabled
		}
	}
	if best >= 0 {
		return allowed
	}

	// No rule claimed this kind. Absence is not a decision to silence — it is no decision at all,
	// and a kind nobody has ruled on behaves exactly as it did before this mechanism existed.
	return true
}
