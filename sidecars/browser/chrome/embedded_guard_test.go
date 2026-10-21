// §spec pilar-de-browser

package chrome

import (
	"context"
	"errors"
	"slices"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

func TestFailedReattachKeepsRecordingForTheRestOfThePersonsTurn(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	if err := driver.DetachEmbedded(context.Background(), id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	emitPaused(t, fake, []map[string]any{pausedAs("G1", "F1", "https://example.org/one", "Document")})
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return nil, errors.New("tab gone")
	})
	if _, err := driver.ReattachEmbedded(context.Background(), id); err == nil {
		t.Fatal("reattach succeeded")
	}
	emitPaused(t, fake, []map[string]any{pausedAs("G2", "F1", "https://idp.example.net/login", "Document")})
	returned := reattachWith(t, fake, driver, id, "SE2")
	want := []string{"https://example.org/one", "https://idp.example.net/login"}
	if !slices.Equal(returned.Chain, want) {
		t.Errorf("chain = %v, want %v (recording must survive a failed reattach)", returned.Chain, want)
	}
}

func TestPageVerbsRefuseADetachedEmbeddedSession(t *testing.T) {
	_, driver, id := chainEmbedded(t)
	ctx := context.Background()
	if err := driver.DetachEmbedded(ctx, id); err != nil {
		t.Fatalf("detach: %v", err)
	}
	if _, err := driver.Act(ctx, id, browser.Action{Kind: "click", Ref: "e1"}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("Act err = %v", err)
	}
	if _, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("Snapshot err = %v", err)
	}
	if _, err := driver.Look(ctx, id); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("Look err = %v", err)
	}
	if _, err := driver.Screenshot(ctx, id); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("Screenshot err = %v", err)
	}
	if err := driver.Watch(ctx, id, func(browser.Frame) {}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("Watch err = %v", err)
	}
}

func TestBeginPersonRefusesAnEmbeddedDriver(t *testing.T) {
	fake, driver, id := chainEmbedded(t)
	before := len(fake.Calls())
	if err := driver.BeginPerson(context.Background(), id); !errors.Is(err, browser.ErrUnsupported) {
		t.Fatalf("BeginPerson err = %v, want ErrUnsupported", err)
	}
	if n := len(fake.Calls()); n != before {
		t.Errorf("BeginPerson sent %d calls", n-before)
	}
	if driver.person.Load() != nil {
		t.Error("person state was installed")
	}
}
