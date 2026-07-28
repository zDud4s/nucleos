package pipe

import (
	"os"
	"regexp"
	"strings"
	"testing"
)

// Feed entries now carry summaries written by a language model over text a stranger sent (the email
// pillar, spec §5.5). This sidecar has both a plain and an HTML path, and the notifier must keep
// using the plain one: sent as HTML, markup injected into a mail body would render as markup in the
// user's chat instead of appearing as the literal text it is.
//
// That is one line, and without this test it is guarded by nothing but a sentence in a document.
func TestFeedNotificationsAreSentAsPlainText(t *testing.T) {
	source, err := os.ReadFile("pipe.go")
	if err != nil {
		t.Fatal(err)
	}

	// Every line that both formats a feed entry and sends it — the function that defines
	// `formatFeed` is not one of them.
	line := regexp.MustCompile(`(?m)^.*bot\.Send.*formatFeed\(.*$`)
	matches := line.FindAllString(string(source), -1)
	if len(matches) == 0 {
		t.Fatal("no feed notification call found — has the notifier moved?")
	}

	for _, match := range matches {
		if strings.Contains(match, "SendHTML") {
			t.Fatalf("feed notifications must not be sent as HTML: %s", strings.TrimSpace(match))
		}
		if !strings.Contains(match, "SendMessage") {
			t.Fatalf("feed notifications must go through SendMessage: %s", strings.TrimSpace(match))
		}
	}
}
