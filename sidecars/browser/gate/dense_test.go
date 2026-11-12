//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAnUnlabelledFieldInATableIsReachable.
//
// The case the tidy fixtures could not show, found by looking at a picture of a crowded page rather
// than by reading a test. `<td>Cabo HDMI</td><td><input value=1></td>` is what an application's table
// looks like, the input has no label, and the reading used to drop it entirely: no ref, so no way to
// type in it, and no line saying a field was there at all.
//
// Against real Chromium rather than a fixture tree, because the question is what CHROMIUM's
// accessibility tree calls an unlabelled input in a cell. The unit test beside this one asserts the
// rule; this asserts that the rule is about the nodes Chrome actually produces.
func TestAnUnlabelledFieldInATableIsReachable(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/dense"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	reading, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	// The five quantity boxes, each holding what the page put in it and each with a ref.
	var boxes []browser.Element
	for _, element := range reading.Elements {
		if element.Role == "textbox" && element.Name == "" && element.Ref != "" {
			boxes = append(boxes, element)
		}
	}
	if len(boxes) != 5 {
		t.Fatalf("unlabelled boxes with refs = %d, want the table's five: %+v",
			len(boxes), reading.Elements)
	}

	// And the rows say what is in them. A row with a gap where the field is hides the field from an
	// agent reading the page in order, which is how it reads every page.
	var wanted string
	for _, element := range reading.Elements {
		if element.Role == "row" && strings.HasPrefix(element.Name, "Cabo HDMI") {
			wanted = element.Name
		}
	}
	if !strings.Contains(wanted, "Cabo HDMI | 1 |") {
		t.Fatalf("row = %q, want the box's contents between the item and the button", wanted)
	}

	// The proof that a ref is a handle and not a label: typing into one lands.
	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionType, Ref: boxes[0].Ref, Text: "42",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("typing into the unlabelled box was refused: %+v", result.Refusal)
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot after: %v", err)
	}
	for _, element := range after.Elements {
		if element.Ref == boxes[0].Ref {
			if !strings.Contains(element.Value, "42") {
				t.Fatalf("the box did not take what was typed into it: %+v", element)
			}
			return
		}
	}
	t.Fatalf("the box lost its ref across an act on it: %+v", after.Elements)
}

// TestACrowdedPageIsLabelledWithoutRunningOutOfRoom.
//
// A look is only worth its cost if the labels can be READ, and "was it labelled" is a question that
// passes whether or not they can be. This pins the two things that would make a crowded page useless:
// every ref the reading handed out gets drawn, and the picture stays small enough to be worth
// sending.
//
// The fixture is a toolbar of buttons a pixel apart, a form, and a table with a control per row —
// about thirty labels, which is an ordinary application screen and a quarter of the label budget.
func TestACrowdedPageIsLabelledWithoutRunningOutOfRoom(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/dense"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	reading, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	look, err := driver.Look(ctx, session.ID)
	if err != nil {
		t.Fatalf("look: %v", err)
	}

	refs := map[string]bool{}
	for _, element := range reading.Elements {
		if element.Ref != "" {
			refs[element.Ref] = true
		}
	}
	if len(refs) < 25 {
		t.Fatalf("the fixture stopped being crowded: %d refs", len(refs))
	}

	drawn := map[string]bool{}
	for _, label := range look.Labels {
		drawn[label] = true
	}
	for ref := range refs {
		if !drawn[ref] {
			t.Fatalf("%s is on screen and in the reading but was not drawn, so the picture and the "+
				"reading disagree about what is there: drew %v", ref, look.Labels)
		}
	}

	// Every label on this page fits in one viewport, so a picture of it should cost tens of
	// kilobytes and not hundreds. The number is loose on purpose: it is a guard against the quality
	// or the format quietly changing, not a measurement of either.
	if size := len(look.Image); size > 200_000 {
		t.Fatalf("the picture is %d bytes of base64 for one crowded viewport, which is a whole "+
			"turn's context for one look", size)
	}
}
