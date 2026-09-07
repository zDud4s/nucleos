package notifier

import "testing"

// The two-layer resolution, exercised where it can actually go wrong.
//
// The synthetic pair `job_` / `job_item_` is deliberate and is NOT a stand-in for real families.
// None of the ten prefixes the núcleo ships is a prefix of another, so a table written with real
// names would never reach the longest-prefix tie-break at all — it would pass while testing
// nothing, which is the worst kind of green.
func TestAllowsResolution(t *testing.T) {
	policy := Policy{
		Families: []Rule{
			{Selector: "job_", Enabled: false},
			{Selector: "job_item_", Enabled: true},
			{Selector: "worktree_", Enabled: true},
		},
		Kinds: []Rule{
			{Selector: "job_failed", Enabled: true},
			{Selector: "worktree_gc", Enabled: false},
		},
	}

	cases := []struct {
		name string
		kind string
		want bool
	}{
		{"a kind rule beats the family that silenced it", "job_failed", true},
		{"a kind rule beats the family that allowed it", "worktree_gc", false},
		{"a kind with no exception follows its family", "job_started", false},
		{"the longest matching prefix wins, not the first", "job_item_done", true},
		{"an allowing family allows", "worktree_pruned", true},
		{"a kind no rule claims passes", "council_opened", true},
		{"a prefix that is not at the start does not match", "nightly_job_started", true},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := policy.Allows(c.kind); got != c.want {
				t.Fatalf("Allows(%q) = %v, want %v", c.kind, got, c.want)
			}
		})
	}

	// The same tie-break with the rules in the OTHER order. Matching families
	// always form a chain, so with only the short-first listing above an
	// implementation that dropped the length tracking and just overwrote on each
	// match would pass every case — "longest wins" and "last wins" agree there.
	// Reversed, they disagree, and only one of them is the rule.
	reversed := Policy{
		Families: []Rule{
			{Selector: "job_item_", Enabled: true},
			{Selector: "job_", Enabled: false},
		},
	}
	if !reversed.Allows("job_item_done") {
		t.Fatal("the longer prefix lost to the one listed after it")
	}
	if reversed.Allows("job_started") {
		t.Fatal("a kind the longer prefix does not claim escaped its own family")
	}
}

// The three contracts from the design, each of which the obvious implementation breaks.
func TestAllowsContracts(t *testing.T) {
	t.Run("the zero value passes everything", func(t *testing.T) {
		// This is also what a failed read produces. A mechanism against noise must not fail into
		// silence, so this case is the failure mode, not an edge case.
		var empty Policy
		for _, kind := range []string{"job_failed", "web.fetched", "anything_at_all"} {
			if !empty.Allows(kind) {
				t.Fatalf("the zero-value policy silenced %q", kind)
			}
		}
	})

	t.Run("an empty kind passes", func(t *testing.T) {
		// An unreadable row is not a row to hush; it is a row that could not be read, and the
		// honest answer to a decision you cannot make is to leave the line alone.
		policy := Policy{Families: []Rule{{Selector: "job_", Enabled: false}}}
		if !policy.Allows("") {
			t.Fatal("an unreadable feed row was silenced")
		}
	})

	t.Run("a rule with an empty selector is ignored", func(t *testing.T) {
		// The empty prefix matches EVERY kind, so honouring one here mutes the channel entirely.
		// The núcleo refuses it at the door; this is the second half of a defence that is worth
		// having on both sides, because the cost of missing it is total silence.
		muted := Policy{
			Families: []Rule{{Selector: "", Enabled: false}},
			Kinds:    []Rule{{Selector: "", Enabled: false}},
		}
		for _, kind := range []string{"job_failed", "council_opened", ""} {
			if !muted.Allows(kind) {
				t.Fatalf("an empty selector silenced %q", kind)
			}
		}
	})
}
