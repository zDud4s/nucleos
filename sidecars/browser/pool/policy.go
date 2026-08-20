package pool

import (
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

// Policy turns the núcleo's decision into the fence the browser will enforce.
//
// # Why there are two enums for one idea, and why that is survivable
//
// profile.Kind says which directory; fence.ProfileKind says which rule. They are the same
// distinction seen from two sides, and keeping them apart means neither package has to import the
// other — fence stays pure and profile stays about disk. The price is this function, and a mapping
// bug here would be a session running under the wrong rule.
//
// What makes that price acceptable is that the mistake cannot be quiet. fence.Policy.Validate
// refuses a project profile with an empty list and refuses an ephemeral one with any list at all, so
// mapping either kind to the other turns every affected open into a loud ErrFenceNotAttached rather
// than a browser that admits the wrong document. The test for this function asserts exactly that
// pairing, in both directions.
func Policy(placement browser.Placement) (fence.Policy, error) {
	if err := placement.Profile.Validate(); err != nil {
		return fence.Policy{}, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	var kind fence.ProfileKind
	switch placement.Profile.Kind {
	case profile.Project:
		kind = fence.Project
	case profile.Ephemeral:
		kind = fence.Ephemeral
	default:
		// Unreachable while Validate is the first thing this function does, and written out anyway:
		// a kind added to profile and forgotten here must not fall through to whichever fence rule
		// happens to be the zero value.
		return fence.Policy{}, fmt.Errorf("%w: no fence rule for profile kind %q",
			browser.ErrFenceNotAttached, placement.Profile.Kind)
	}

	policy := fence.Policy{
		Profile: kind,
		Origins: placement.Origins,
		// Loopback stays empty, which means the fence admits none of it. Spec §6.2's loopback rule
		// exists because this machine's own services — the núcleo's API, the other sidecars, and the
		// browser's own token-less debugging port — are reachable from a page unless something
		// refuses them. Nothing the núcleo can send opens that list today, and that is deliberate:
		// there is no browsing reason to reach them, so the list is a seam and not a setting.
	}
	if err := policy.Validate(); err != nil {
		return fence.Policy{}, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}
	return policy, nil
}
