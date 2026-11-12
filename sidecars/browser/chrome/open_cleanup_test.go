// §spec pilar-de-browser

package chrome

import (
	"context"
	"errors"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// TestAFailedOpenClosesWhatItOpened. Open creates a target before it can fail, and a tab left behind
// by an open that returned an error belongs to nobody: no session id was handed out to close it by.
func TestAFailedOpenClosesWhatItOpened(t *testing.T) {
	for _, failing := range []string{"Page.enable", "Page.navigate"} {
		t.Run(failing, func(t *testing.T) {
			fake, conn := dial(t)
			autoAttachOnCreate(fake)
			fake.Handle("Target.closeTarget", func(cdptest.Call) (any, error) { return map[string]any{}, nil })
			driver, err := Connect(context.Background(), conn, projectPolicy())
			if err != nil {
				t.Fatalf("connect: %v", err)
			}
			fake.Handle(failing, func(cdptest.Call) (any, error) { return nil, errors.New("boom") })

			if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err == nil {
				t.Fatal("open succeeded although " + failing + " failed")
			}
			// The startup sweep closes a target of its own, so only a close AFTER Page.enable (the
			// first call on the page Open made) counts.
			methods := fake.Methods()
			closedAfter := false
			for _, m := range methods[max(fake.IndexOf("Page.enable"), 0):] {
				closedAfter = closedAfter || m == "Target.closeTarget"
			}
			if !closedAfter {
				t.Errorf("the target survived a failed open: %v", methods)
			}
			driver.mu.Lock()
			left := len(driver.sessions)
			driver.mu.Unlock()
			if left != 0 {
				t.Errorf("%d session(s) left registered after a failed open", left)
			}
		})
	}
}
