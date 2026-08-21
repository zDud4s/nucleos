//go:build browsergate

package gate_test

import (
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"image"
	_ "image/jpeg"
	"net/url"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestALookLabelsWhatTheReadingNamed.
//
// The claim the whole verb rests on: the numbers drawn on the picture are the refs the agent already
// holds, so seeing something and acting on it are the same vocabulary. A picture with its own
// numbering would be a second naming scheme the agent has to translate — and translating between two
// namings of the same page, from an image, is exactly the operation a model gets wrong quietly.
func TestALookLabelsWhatTheReadingNamed(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/canvas"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	before, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	look, err := driver.Look(ctx, session.ID)
	if err != nil {
		t.Fatalf("look: %v", err)
	}
	if look.Image == "" {
		t.Fatal("the look came back with no picture at all")
	}
	if look.MIME != "image/jpeg" {
		t.Fatalf("mime = %q, want image/jpeg", look.MIME)
	}
	if look.Width <= 0 || look.Height <= 0 {
		t.Fatalf("the picture reports no size: %dx%d", look.Width, look.Height)
	}
	if _, _, err := image.Decode(bytes.NewReader(decoded(t, look.Image))); err != nil {
		t.Fatalf("the picture does not decode as an image: %v", err)
	}

	known := map[string]bool{}
	for _, element := range before.Elements {
		if element.Ref != "" {
			known[element.Ref] = true
		}
	}
	if len(look.Labels) == 0 {
		t.Fatal("nothing was labelled on a page whose reading showed controls")
	}
	for _, label := range look.Labels {
		if !known[label] {
			t.Fatalf("the picture is labelled %q, which the reading never handed out: reading had %v",
				label, sortedRefs(known))
		}
	}
}

// TestALookOfAPageNobodyHasReadHasNoLabels.
//
// **The control, and without it the test above proves much less than it appears to.** A design that
// numbered elements by their position on screen would pass every assertion up there — the numbers
// would look like refs and would even collide with them on a simple page — and would be wrong in the
// exact way this pillar has already been wrong once: minting by position renames everything below an
// element the moment a row is inserted.
//
// A page nobody has read has no refs, so a labelling that comes FROM the refs has nothing to draw,
// while a labelling that comes from the page draws everything. The two designs disagree here and
// nowhere else.
func TestALookOfAPageNobodyHasReadHasNoLabels(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/focus"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	look, err := driver.Look(ctx, session.ID)
	if err != nil {
		t.Fatalf("look: %v", err)
	}
	if look.Image == "" {
		t.Fatal("a page nobody has read is still a page, and should still be photographed")
	}
	if len(look.Labels) != 0 {
		t.Fatalf("labels were drawn on a page no snapshot has named: %v — the numbering is coming"+
			" from the page rather than from the refs", look.Labels)
	}
}

// TestALookLeavesTheDocumentAsItFoundIt.
//
// An overlay left behind is not cosmetic. It is DOM the next snapshot reads as content — and the
// aria-hidden that keeps it out of the accessibility tree is exactly what would hide it from anyone
// trying to work out why the page grew a hundred divs.
//
// Asked of the PAGE and not of a snapshot, and that distinction is the test. A snapshot cannot see
// an aria-hidden element, so a reading taken afterwards would come back clean whether or not the
// overlay was still there — it would pass against precisely the bug it was written for.
func TestALookLeavesTheDocumentAsItFoundIt(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/lookclean"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	before, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	ask := refFor(t, before, "Ask")

	if _, err := driver.Look(ctx, session.ID); err != nil {
		t.Fatalf("look: %v", err)
	}

	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: ask})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("the button that asks the page about itself was refused: %+v", result.Refusal)
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot after: %v", err)
	}
	said := allText(after)
	if !strings.Contains(said, "overlay is gone") {
		t.Fatalf("the page says the overlay outlived the look, so the next reading of it is reading"+
			" our own drawing: %s", said)
	}

	// And the refs did not move. The overlay is injected into the same documents the refs point
	// into, so a look that renumbered the page would leave the agent holding names for other things.
	if refFor(t, after, "Ask") != ask {
		t.Fatalf("the ref for the same button changed across a look: %s then %s",
			ask, refFor(t, after, "Ask"))
	}
}

// TestALabelInsideACrossSiteFrameLandsWhereTheFrameIs.
//
// **The measurement the whole design was betting on, and it is about Chromium rather than about this
// code.** A cross-site frame is a separate renderer, and where Chromium has decided to draw it is
// the one thing process isolation hides from everybody outside it. The bet is that a label drawn
// INSIDE that frame, in the frame's own coordinates, is composed into the top target's screenshot at
// the right place — because then the correctness comes free from the same mechanism that created the
// problem.
//
// The assertion is on pixels, and it has to be: "the frame was labelled" would pass with the label
// drawn at the top-left corner of the page, which is what an implementation that ignored the frame
// offset would produce. The coordinates come from the two pages' own stylesheets — the frame is at
// (200,150) and the button is at (20,30) inside it — so the button sits at (220,180) and its label
// box, which is drawn fifteen pixels above, at (220,165).
//
// MEASURED, and the number came out exact: the ink starts at (220,165) with nothing to round. That
// is the whole finding — a CSS pixel inside an out-of-process frame is the same pixel in the top
// target's screenshot, so the frame offset never has to be computed on our side, which is fortunate
// because it is precisely what process isolation refuses to tell us.
//
// Absolute pixels rather than a fraction of the picture, and the first version of this test had it
// the other way round for fear of depending on the viewport. That fear pointed at the wrong risk:
// the headless viewport is NOT 800x600 (it measured 762x484 here), so a fraction is the thing that
// varies while the pixel is the thing that does not. What matters is that the button is inside the
// viewport at all, which at 320x220 it comfortably is.
func TestALabelInsideACrossSiteFrameLandsWhereTheFrameIs(t *testing.T) {
	site := newSite(t)
	policy := admitting(site)
	policy.Loopback = append(policy.Loopback, otherHost(site))
	driver, _ := fenced(t, policy)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/lookframe?src=" + url.QueryEscape(otherHost(site)+"/lookbutton"),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal != nil {
		t.Fatalf("the framing page itself was refused: %+v", session.Refusal)
	}

	// Polled for the reason the other cross-frame tests poll: a reading taken the instant Open
	// returns is racing the frame's own load, which is a different question from this one.
	var labels []string
	var look browser.LookResult
	deadline := time.Now().Add(30 * time.Second)
	for {
		if _, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{}); err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		look, err = driver.Look(ctx, session.ID)
		if err != nil {
			t.Fatalf("look: %v", err)
		}
		labels = look.Labels
		if len(labels) > 0 || time.Now().After(deadline) {
			break
		}
		time.Sleep(500 * time.Millisecond)
	}
	if len(labels) == 0 {
		t.Fatal("the button inside the cross-site frame was never labelled, so a look stops at the" +
			" process boundary the snapshot already crosses")
	}

	left, top, found := inkBounds(t, decoded(t, look.Image))
	if !found {
		t.Fatal("the picture carries no label ink at all, so the overlay did not reach the" +
			" screenshot even though the frame reported drawing it")
	}
	// A few pixels of slack for the JPEG, which bleeds a hard magenta edge outwards by a pixel or
	// two — and for nothing else. An implementation that drew in the FRAME's coordinate space
	// without the offset would land at (20,15), two hundred pixels away, so the tolerance never has
	// to be large to tell the two apart.
	const wantX, wantY, slack = 220, 165, 4
	if left < wantX-slack || left > wantX+slack || top < wantY-slack || top > wantY+slack {
		t.Fatalf("the label's ink starts at (%d,%d) and the framed button's label belongs at"+
			" (%d,%d) in a %dx%d picture: the overlay was composed in the wrong coordinate space",
			left, top, wantX, wantY, look.Width, look.Height)
	}
}

// inkBounds finds the top-left corner of the overlay's own colour in a picture.
//
// A colour test rather than a shape test, and a wide one: the picture is JPEG, so every pixel of a
// flat fill comes back slightly wrong, and the ringing around a hard edge is worse than the fill.
// What it has to separate is a magenta label from a white page, which survives any amount of that.
func inkBounds(t *testing.T, data []byte) (int, int, bool) {
	t.Helper()
	picture, _, err := image.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("decoding the picture: %v", err)
	}
	box := picture.Bounds()
	left, top, found := box.Max.X, box.Max.Y, false
	for y := box.Min.Y; y < box.Max.Y; y++ {
		for x := box.Min.X; x < box.Max.X; x++ {
			r, g, b, _ := picture.At(x, y).RGBA()
			// #ff007f, loosely: strongly red, barely green, middling blue.
			if r>>8 > 180 && g>>8 < 110 && b>>8 > 50 && b>>8 < 170 {
				found = true
				if x < left {
					left = x
				}
				if y < top {
					top = y
				}
			}
		}
	}
	return left, top, found
}

func decoded(t *testing.T, encoded string) []byte {
	t.Helper()
	data, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		t.Fatalf("the picture is not base64: %v", err)
	}
	return data
}

// allText is everything the reading said, for an assertion about what a page now says about itself.
func allText(snapshot browser.Snapshot) string {
	var said strings.Builder
	for _, element := range snapshot.Elements {
		fmt.Fprintf(&said, "%s %s %s\n", element.Role, element.Name, element.Value)
	}
	return said.String()
}

func sortedRefs(known map[string]bool) []string {
	refs := make([]string, 0, len(known))
	for ref := range known {
		refs = append(refs, ref)
	}
	return refs
}
