// §spec email-pillar

package imap

import (
	"os"
	"strings"
	"testing"
)

// The invariant this whole pillar rests on (spec §3.1): the sidecar never modifies the mailbox.
//
// Asserted against the source rather than against a server, deliberately. A command-recording fake
// server would only prove that today's code paths are clean; this fails the moment ANY mutating
// verb appears in the package, including on a path no test happens to exercise. The user is
// calibrating this against a real inbox — the cost of being wrong is their mail, and it is not
// recoverable by apologising.
func TestNoMutatingCommandExistsInThisPackage(t *testing.T) {
	source, err := os.ReadFile("imap.go")
	if err != nil {
		t.Fatal(err)
	}
	text := string(source)

	// Written as call sites, not bare words, so the comments explaining the invariant do not trip
	// their own test.
	for _, forbidden := range []string{
		".Store(", ".Move(", ".Copy(", ".Expunge(", ".UIDStore(", ".UIDMove(",
		".UIDCopy(", ".UIDExpunge(", ".Append(", ".Delete(", ".Rename(", ".Create(",
	} {
		if strings.Contains(text, forbidden) {
			t.Fatalf("%s must never appear here: the mailbox is read-only", forbidden)
		}
	}
}

// Two positive halves of the same invariant, because their absence is silent: a writable SELECT
// lets the server set \Seen on fetch, and a fetch without PEEK marks mail as read even under a
// read-only select on some servers.
func TestTheMailboxIsOpenedReadOnlyAndFetchedWithPeek(t *testing.T) {
	source, err := os.ReadFile("imap.go")
	if err != nil {
		t.Fatal(err)
	}
	text := string(source)

	if !strings.Contains(text, "ReadOnly: true") {
		t.Fatal("the mailbox must be selected read-only")
	}
	if !strings.Contains(text, "Peek: true") {
		t.Fatal("bodies must be read with BODY.PEEK so fetching does not mark mail as read")
	}
}
