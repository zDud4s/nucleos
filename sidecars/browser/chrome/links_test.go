package chrome

import (
	"testing"

	"nucleosbrowser/browser"
)

func linkTo(id, name, href string) axNode {
	return axNode{
		NodeID: id, Role: axValue{Value: "link"}, Name: axValue{Value: name},
		BackendDOMNodeID: 700,
		Properties:       []axProperty{{Name: "url", Value: axValue{Value: href}}},
	}
}

// TestALinkSaysWhereItGoes.
//
// Without this two links called "Details" are the same link, and a link that opens in a new window —
// which the fence refuses — has no way onward at all: `goto` is the answer and `goto` needs an
// address no reading had ever given.
func TestALinkSaysWhereItGoes(t *testing.T) {
	nodes := []axNode{linkTo("1", "Details", "https://example.org/invoices/42")}
	collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{})

	if len(collected) != 1 {
		t.Fatalf("expected the link: %+v", collected)
	}
	if got := collected[0].element.URL; got != "/invoices/42" {
		t.Fatalf("the link's address came back as %q", got)
	}
}

// TestAnAddressIsShortenedOnlyWhereNothingIsLost.
//
// A path is the same address when it points at the page's own origin, and `goto` resolves one
// against the page. Somewhere else is where the whole address is the news.
func TestAnAddressIsShortenedOnlyWhereNothingIsLost(t *testing.T) {
	for _, one := range []struct {
		raw  string
		page string
		want string
	}{
		{raw: "https://example.org/a/b", page: "https://example.org/here", want: "/a/b"},
		{raw: "https://example.org/a?q=1#x", page: "https://example.org/here", want: "/a?q=1#x"},
		{raw: "https://example.org", page: "https://example.org/here", want: "/"},
		{raw: "https://elsewhere.net/a", page: "https://example.org/here", want: "https://elsewhere.net/a"},
		// A different port is a different service on the same machine, which browser_policy.rs is
		// explicit about; shortening across one would hide the only part that differed.
		{raw: "https://example.org:8443/a", page: "https://example.org/here", want: "https://example.org:8443/a"},
		{raw: "http://example.org/a", page: "https://example.org/here", want: "http://example.org/a"},
		{raw: "/already/relative", page: "https://example.org/here", want: "/already/relative"},
		{raw: "mailto:someone@example.org", page: "https://example.org/here", want: "mailto:someone@example.org"},
		{raw: "", page: "https://example.org/here", want: ""},
		{raw: "https://example.org/a", page: "", want: "https://example.org/a"},
	} {
		if got := shortURL(one.raw, one.page); got != one.want {
			t.Errorf("%q against %q became %q, wanted %q", one.raw, one.page, got, one.want)
		}
	}
}

// TestWhatHasFocusIsSaid.
//
// `press` with no ref sends its key to whatever has focus. Before this the reading never named that
// element, so the one verb whose target the agent cannot choose was also the one whose target the
// agent could not see — a key going somewhere, and the page's answer read as the page's behaviour.
func TestWhatHasFocusIsSaid(t *testing.T) {
	node := axNode{
		Role: axValue{Value: "textbox"}, Name: axValue{Value: "Search"},
		Properties: []axProperty{{Name: "focused", Value: axValue{Value: "true"}}},
	}
	state := stateOf(node)
	found := false
	for _, one := range state {
		if one == "focused" {
			found = true
		}
	}
	if !found {
		t.Fatalf("the focused box did not say so: %v", state)
	}

	// And the other half: an element that is not focused says nothing, rather than saying it is not.
	// One state string per element per snapshot is a real cost, and "not focused" is every element
	// on the page.
	quiet := axNode{
		Role: axValue{Value: "textbox"}, Name: axValue{Value: "Other"},
		Properties: []axProperty{{Name: "focused", Value: axValue{Value: "false"}}},
	}
	for _, one := range stateOf(quiet) {
		if one == "focused" {
			t.Fatal("every element that is not focused would say so, on every snapshot")
		}
	}
}
