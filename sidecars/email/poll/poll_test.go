package poll

import (
	"errors"
	"reflect"
	"testing"
	"time"

	imapv2 "github.com/emersion/go-imap/v2"

	"nucleosemail/config"
	"nucleosemail/extract"
	"nucleosemail/imap"
)

// fakeFetcher serves scripted messages and can die at a chosen uid, which is the only behaviour
// the watermark rule actually depends on.
type fakeFetcher struct {
	bodies  map[uint32][]byte
	dieAt   uint32
	fetched []uint32
}

func (f *fakeFetcher) Fetch(uid imapv2.UID) (imap.Raw, error) {
	f.fetched = append(f.fetched, uint32(uid))
	if f.dieAt != 0 && uint32(uid) == f.dieAt {
		return imap.Raw{}, errors.New("connection reset")
	}
	body, ok := f.bodies[uint32(uid)]
	if !ok {
		return imap.Raw{}, errors.New("no such message")
	}
	return imap.Raw{
		UID:          uint32(uid),
		InternalDate: time.Date(2026, 7, 28, 10, 0, 0, 0, time.UTC),
		Body:         body,
	}, nil
}

func validMessage(subject string) []byte {
	return []byte("From: Ana <ana@company.com>\r\n" +
		"Subject: " + subject + "\r\n" +
		"Message-ID: <" + subject + "@company.com>\r\n" +
		"Content-Type: text/plain\r\n\r\n" +
		"body text\r\n")
}

func uidRange(from, to uint32) []imapv2.UID {
	var uids []imapv2.UID
	for uid := from; uid <= to; uid++ {
		uids = append(uids, imapv2.UID(uid))
	}
	return uids
}

func TestACycleContinuesAfterOneMailboxFails(t *testing.T) {
	failure := errors.New("mailbox unavailable")
	tests := []struct {
		name    string
		targets []Target
	}{
		{
			name: "inbox fails first",
			targets: []Target{
				{Mailbox: "INBOX", Direction: "inbound"},
				{Mailbox: "Sent Items", Direction: "outbound"},
			},
		},
		{
			name: "sent fails first",
			targets: []Target{
				{Mailbox: "Sent Items", Direction: "outbound"},
				{Mailbox: "INBOX", Direction: "inbound"},
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var called []Target
			errs := Cycle(test.targets, func(target Target) error {
				called = append(called, target)
				if len(called) == 1 {
					return failure
				}
				return nil
			})

			if !reflect.DeepEqual(called, test.targets) {
				t.Fatalf("polled %v, want every target in order %v", called, test.targets)
			}
			if len(errs) != 1 || !errors.Is(errs[0], failure) {
				t.Fatalf("errors = %v, want exactly the first target's failure", errs)
			}
		})
	}
}

func TestTargetsOmitsSentWhenUnconfigured(t *testing.T) {
	withoutSent := Targets(config.Config{Mailbox: "INBOX"})
	wantWithoutSent := []Target{{Mailbox: "INBOX", Direction: "inbound"}}
	if !reflect.DeepEqual(withoutSent, wantWithoutSent) {
		t.Fatalf("Targets without sent mailbox = %v, want %v", withoutSent, wantWithoutSent)
	}

	withSent := Targets(config.Config{
		Mailbox:     "INBOX",
		SentMailbox: "[Gmail]/Sent Mail",
	})
	wantWithSent := []Target{
		{Mailbox: "INBOX", Direction: "inbound"},
		{Mailbox: "[Gmail]/Sent Mail", Direction: "outbound"},
	}
	if !reflect.DeepEqual(withSent, wantWithSent) {
		t.Fatalf("Targets with sent mailbox = %v, want %v", withSent, wantWithSent)
	}
}

// The reading that would lose mail: reporting the highest uid the SEARCH returned rather than the
// highest one actually read moves the núcleo's cursor over everything the dead connection never
// delivered.
func TestFetchDyingMidwayReportsWhatItRead(t *testing.T) {
	bodies := map[uint32][]byte{}
	for uid := uint32(1); uid <= 50; uid++ {
		bodies[uid] = validMessage("m")
	}
	fetcher := &fakeFetcher{bodies: bodies, dieAt: 12}

	messages, _, maxExamined := Collect(fetcher, uidRange(1, 50), MaxPerBatch, extract.Message)

	if maxExamined != 11 {
		t.Fatalf("max examined = %d, want 11 (the last uid actually read)", maxExamined)
	}
	if len(messages) != 11 {
		t.Fatalf("delivered %d messages, want 11", len(messages))
	}
}

// A message nobody can parse is a decision, not a failure: it is reported and the cursor passes it,
// or one corrupt message stalls the mailbox forever.
func TestACorruptMessageIsSkippedNotStalled(t *testing.T) {
	fetcher := &fakeFetcher{bodies: map[uint32][]byte{
		1: validMessage("first"),
		2: []byte("this is not a message at all"),
		3: validMessage("third"),
	}}

	messages, skipped, maxExamined := Collect(fetcher, uidRange(1, 3), MaxPerBatch, extract.Message)

	if len(messages) != 2 {
		t.Fatalf("delivered %d messages, want 2", len(messages))
	}
	if len(skipped) != 1 || skipped[0].UID != 2 {
		t.Fatalf("skipped = %+v, want exactly uid 2", skipped)
	}
	if maxExamined != 3 {
		t.Fatalf("max examined = %d, want 3 — a skip is still a decision", maxExamined)
	}
}

func TestABatchIsPagedNotDropped(t *testing.T) {
	bodies := map[uint32][]byte{}
	for uid := uint32(1); uid <= 250; uid++ {
		bodies[uid] = validMessage("m")
	}
	fetcher := &fakeFetcher{bodies: bodies}

	messages, _, maxExamined := Collect(fetcher, uidRange(1, 250), MaxPerBatch, extract.Message)

	if len(messages) != MaxPerBatch {
		t.Fatalf("delivered %d messages, want %d", len(messages), MaxPerBatch)
	}
	if maxExamined != MaxPerBatch {
		t.Fatalf("max examined = %d, want %d", maxExamined, MaxPerBatch)
	}
	if len(fetcher.fetched) != MaxPerBatch {
		t.Fatalf("fetched %d, want the page to stop at %d", len(fetcher.fetched), MaxPerBatch)
	}
}

// The first message failing means nothing was read all the way through, so nothing may move.
func TestAnImmediateFailureExaminesNothing(t *testing.T) {
	fetcher := &fakeFetcher{bodies: map[uint32][]byte{}, dieAt: 1}
	messages, skipped, maxExamined := Collect(
		fetcher,
		uidRange(1, 5),
		MaxPerBatch,
		extract.Message,
	)
	if len(messages) != 0 || len(skipped) != 0 || maxExamined != 0 {
		t.Fatalf("got %d messages, %d skipped, watermark %d — want nothing",
			len(messages), len(skipped), maxExamined)
	}
}

// A mailbox with nothing new answers `SearchAbove` with its own last message, because IMAP's `n:*`
// range always matches the highest uid present. Left in, it makes every idle poll claim there is
// one message to read.
func TestStrictlyAboveDropsTheCursorsOwnMessage(t *testing.T) {
	kept := StrictlyAbove([]imapv2.UID{8239}, 8239)
	if len(kept) != 0 {
		t.Fatalf("kept %v, want nothing above the cursor", kept)
	}
}

func TestStrictlyAboveKeepsGenuinelyNewMail(t *testing.T) {
	kept := StrictlyAbove([]imapv2.UID{8239, 8240, 8241}, 8239)
	if len(kept) != 2 || kept[0] != 8240 || kept[1] != 8241 {
		t.Fatalf("kept %v, want [8240 8241]", kept)
	}
}

// No cursor means no floor, and uid 0 is not a real uid — every message is new.
func TestStrictlyAboveKeepsEverythingWithoutACursor(t *testing.T) {
	kept := StrictlyAbove([]imapv2.UID{1, 2, 3}, 0)
	if len(kept) != 3 {
		t.Fatalf("kept %v, want all three", kept)
	}
}

func TestTheSentFolderIsReadForItsRecipients(t *testing.T) {
	raw := []byte("From: Ana <ana@example.test>\r\n" +
		"To: Maria <maria@example.test>, oncall@example.test\r\n" +
		"Subject: hello\r\n" +
		"Message-ID: <hello@example.test>\r\n" +
		"Content-Type: text/plain\r\n\r\n" +
		"body text\r\n")
	fetcher := &fakeFetcher{bodies: map[uint32][]byte{1: raw}}

	outbound, _, _ := Collect(fetcher, uidRange(1, 1), MaxPerBatch, ExtractorFor("outbound"))
	inbound, _, _ := Collect(fetcher, uidRange(1, 1), MaxPerBatch, ExtractorFor("inbound"))

	if len(outbound) != 1 || len(inbound) != 1 {
		t.Fatalf("got %d outbound and %d inbound messages, want one of each",
			len(outbound), len(inbound))
	}
	if outbound[0].Headers["to"] == "" {
		t.Fatalf("outbound headers = %+v, want recipients", outbound[0].Headers)
	}
	if _, ok := inbound[0].Headers["to"]; ok {
		t.Fatalf("inbound headers = %+v, must not forward recipients", inbound[0].Headers)
	}
}
