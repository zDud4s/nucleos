// Package reading is the one shape this sidecar reports, and the place where every provider's own
// percentage is converted to a single fraction.
//
// The Anthropic endpoint reports `utilization` as a PERCENTAGE carried in a float field — the name
// reads like a fraction and is not one — while the same payload's `limits[].percent` and
// `seven_day_breakdown.rows[].percent` are integer percentages, and the Codex rollouts carry
// `used_percent`. Three spellings of one percentage is how a ring ends up drawn a hundred times too
// full, so nothing past this package is allowed to see a percentage: every provider converts at its
// own edge and hands back UsedFraction.
package reading

import "time"

// Fidelity says how much the number below is worth, and therefore whether a brake may act on it.
// The ladder is the design's (D3): a reading that was not measured must never stop the owner's
// work, however alarming it looks.
type Fidelity string

const (
	// Official came from the provider's own endpoint. Percentage and reset are the vendor's.
	Official Fidelity = "official"
	// Derived was computed from files the provider left on this machine. True when last written,
	// which is why Window.ReadAt matters more here than for Official.
	Derived Fidelity = "derived"
	// Unmeasured means the provider is configured and no quota source could be read — no token, an
	// expired one, an endpoint that refused. Never a zero; absence and zero per cent are different
	// facts and are not drawn the same way.
	Unmeasured Fidelity = "unmeasured"
)

// Window is one limit period of one provider.
type Window struct {
	// Name is "5h" or "7d". Deliberately the design's vocabulary rather than the vendor's
	// `five_hour`/`seven_day`, so a vendor rename does not reach the shell.
	Name string `json:"window"`
	// UsedFraction is in [0,1]. Never a percentage — see the package comment.
	UsedFraction float64 `json:"used_fraction"`
	// ResetsAt is when this window rolls over. Nil is a real answer, not a defect: the captured
	// payload of 2026-09-19 carried a populated window (`nimbus_quill`) whose reset was null, so a
	// window that never says when it reopens must not crash a caller that assumed it would.
	ResetsAt *time.Time `json:"resets_at"`
	// Stale marks a reading whose ResetsAt is already in the past — the rule `_annotate_stale`
	// applies to the Codex rollouts in the Python dashboard, generalised here to both providers
	// because a window past its own reset is equally meaningless whoever reported it.
	Stale bool `json:"stale"`
}

// Provider is everything known about one assistant's quota right now.
type Provider struct {
	Name     string    `json:"provider"`
	Fidelity Fidelity  `json:"fidelity"`
	ReadAt   time.Time `json:"read_at"`
	Windows  []Window  `json:"windows"`
	// Detail carries why a reading is Unmeasured, for the feed line and the tooltip. Empty
	// otherwise. It never carries a token, a header or a URL with credentials in it.
	Detail string `json:"detail,omitempty"`
	// Severity is the vendor's own word for how bad this is ("normal" in the 2026-09-19 capture).
	// Recorded and never acted on: the design's states come from UsedFraction against the owner's
	// own thresholds, which are configurable, and borrowing the vendor's vocabulary would hand a
	// brake to a word we do not control.
	Severity string `json:"severity,omitempty"`
}

// Unavailable builds the reading for a provider whose quota could not be read at all.
//
// A constructor rather than a literal at each call site, because the invariant is easy to break by
// hand: Unmeasured with a non-empty Windows list would let a caller find a number to believe.
func Unavailable(name, detail string, at time.Time) Provider {
	return Provider{
		Name:     name,
		Fidelity: Unmeasured,
		ReadAt:   at,
		Windows:  nil,
		Detail:   detail,
	}
}

// MarkStale sets Stale on every window whose reset has already passed.
func MarkStale(windows []Window, now time.Time) []Window {
	for i := range windows {
		if windows[i].ResetsAt != nil && windows[i].ResetsAt.Before(now) {
			windows[i].Stale = true
		}
	}
	return windows
}
