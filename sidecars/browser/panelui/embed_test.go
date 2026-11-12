// §spec browser-com-painel

package panelui

import (
	"strings"
	"testing"
)

// TestPanelBundleIsEmbedded. The bundle is what the driver injects into every target of a visible
// session, so a build that embedded an empty file would arm a panel world that shows nothing and fail
// no other test. __nucleosPush is the one name the driver calls into the page by, so a bundle without
// it cannot receive a single message.
func TestPanelBundleIsEmbedded(t *testing.T) {
	if strings.TrimSpace(Source) == "" {
		t.Fatal("the embedded panel bundle is empty")
	}
	if !strings.Contains(Source, "__nucleosPush") {
		t.Error("the embedded panel bundle does not define __nucleosPush, the entry the driver pushes messages through")
	}
}
