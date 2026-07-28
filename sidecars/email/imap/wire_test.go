package imap

import (
	"strings"
	"testing"
	"time"
)

// The invariant, finally asserted where it actually lives: on the wire.
//
// readonly_test.go proves this package contains no mutating call. That is the stronger check for
// code we write — it fails even on a path no test reaches — but it is blind to the one failure it
// cannot see from the source: a client library issuing a command we never asked for. A version
// bump that decided to set \Seen after fetching, or to send a plain SELECT when it thinks it knows
// better, would pass every source scan and quietly mark the user's mail as read.
//
// So this drives a whole poll cycle and reads back everything the server received.
func TestNoMutatingCommandReachesTheServerDuringAFullCycle(t *testing.T) {
	server := newFakeServer(t)
	conn := server.connect(t)

	if _, err := conn.Select("INBOX"); err != nil {
		t.Fatalf("select: %v", err)
	}
	uids, err := conn.SearchAbove(4)
	if err != nil {
		t.Fatalf("search: %v", err)
	}
	if len(uids) == 0 {
		t.Fatal("the fake was supposed to answer with uids")
	}
	if _, err := conn.Fetch(uids[0]); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	conn.Close()

	// Space-prefixed so a mailbox named "COPYWRITING" or a body containing "DELETE" cannot trip
	// this; every one of these is a command verb, and a verb follows a tag or "UID ".
	for _, forbidden := range []string{
		" STORE", " MOVE", " COPY", " EXPUNGE", " APPEND", " DELETE", " RENAME", " CREATE", " SETACL",
	} {
		for _, line := range server.lines() {
			if strings.Contains(line, forbidden) {
				t.Fatalf("the mailbox was modified:%s appeared in %q", forbidden, line)
			}
		}
	}
}

// `ReadOnly: true` is a struct field, and a struct field is a request. This is the check that it
// became EXAMINE — the command that cannot mark mail as read — rather than being dropped by a
// library that renamed the option.
func TestSelectIsExamineAndNeverAWritableSelect(t *testing.T) {
	server := newFakeServer(t)
	conn := server.connect(t)

	uidValidity, err := conn.Select("INBOX")
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if uidValidity != server.uidValidity {
		t.Fatalf("uidvalidity: got %d, want %d", uidValidity, server.uidValidity)
	}

	server.sent(t, "EXAMINE")
	for _, line := range server.lines() {
		// EXAMINE contains no "SELECT", so any SELECT here is a writable one.
		if strings.Contains(line, "SELECT") {
			t.Fatalf("a writable SELECT was sent: %q", line)
		}
	}
}

// The other half, and the one that fails silently: under a read-only select some servers still set
// \Seen on a fetch without PEEK. The user finds their unread mail read and nothing errored.
func TestBodiesAreFetchedWithPeek(t *testing.T) {
	server := newFakeServer(t)
	conn := server.connect(t)

	if _, err := conn.Select("INBOX"); err != nil {
		t.Fatalf("select: %v", err)
	}
	raw, err := conn.Fetch(5)
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}

	fetch := server.sent(t, "UID FETCH")
	if !strings.Contains(fetch, "BODY.PEEK[") {
		t.Fatalf("the body was fetched without PEEK, which marks mail as read: %q", fetch)
	}

	// And the three things only the server can tell us survive the round trip.
	if raw.UID != 5 {
		t.Errorf("uid: got %d, want 5", raw.UID)
	}
	if !raw.InternalDate.Equal(server.internal) {
		t.Errorf("internal date: got %s, want %s", raw.InternalDate, server.internal)
	}
	if string(raw.Body) != string(server.message) {
		t.Errorf("body: got %q, want %q", raw.Body, server.message)
	}
}

// The tail range, which is the shape poll.StrictlyAbove exists to compensate for. Asserting it here
// means the quirk stays documented by a test rather than only by the workaround downstream.
func TestSearchAboveAsksForTheTailStrictlyAfterTheWatermark(t *testing.T) {
	server := newFakeServer(t)
	conn := server.connect(t)
	if _, err := conn.Select("INBOX"); err != nil {
		t.Fatalf("select: %v", err)
	}

	if _, err := conn.SearchAbove(8239); err != nil {
		t.Fatalf("search: %v", err)
	}

	search := server.sent(t, "UID SEARCH")
	// 8240 and not 8239: the watermark is the last uid already ingested.
	if !strings.Contains(search, "8240:*") {
		t.Fatalf("expected the tail above the watermark, got %q", search)
	}
}

// The UIDVALIDITY-changed path, which until now had never been executed by anything. It is the
// branch that runs when the server renumbers the mailbox and the stored watermark becomes a
// statement about a mailbox that no longer exists — the moment a wrong query silently re-reads or
// silently skips everything.
func TestSearchSinceAsksByDateForTheRenumberedMailbox(t *testing.T) {
	server := newFakeServer(t)
	conn := server.connect(t)
	if _, err := conn.Select("INBOX"); err != nil {
		t.Fatalf("select: %v", err)
	}

	since := time.Date(2026, 7, 21, 0, 0, 0, 0, time.UTC)
	uids, err := conn.SearchSince(since)
	if err != nil {
		t.Fatalf("search since: %v", err)
	}
	if len(uids) != len(server.searchUIDs) {
		t.Fatalf("uids: got %d, want %d", len(uids), len(server.searchUIDs))
	}

	// The date arrives quoted — an IMAP astring — so the assertion is on the verb and the date,
	// not on the quoting the encoder happens to choose.
	search := server.sent(t, "UID SEARCH")
	if !strings.Contains(search, "SINCE") || !strings.Contains(search, "21-JUL-2026") {
		t.Fatalf("expected a SINCE date search, got %q", search)
	}
	// A date window must not also carry a uid range, or it would re-apply the watermark it exists
	// to abandon.
	if strings.Contains(search, ":*") {
		t.Fatalf("the date window must not carry a uid range: %q", search)
	}
}

// An empty answer has to survive as an empty answer. A mailbox with nothing new is the common case,
// and turning it into an error would put the sidecar into a restart loop on a healthy inbox.
func TestAnEmptySearchIsNotAnError(t *testing.T) {
	server := newFakeServer(t)
	server.searchUIDs = nil
	conn := server.connect(t)
	if _, err := conn.Select("INBOX"); err != nil {
		t.Fatalf("select: %v", err)
	}

	uids, err := conn.SearchAbove(1)
	if err != nil {
		t.Fatalf("an empty mailbox must not be an error: %v", err)
	}
	if len(uids) != 0 {
		t.Fatalf("expected no uids, got %v", uids)
	}
}
