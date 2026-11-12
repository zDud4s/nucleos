//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAnAttachedFileReachesTheServer.
//
// The whole verb, end to end, with the SERVER as the witness. Everything before this point can be
// true while nothing arrives: the page can accept the file, the fence can allow the POST, the driver
// can report `done`, and the multipart body can still be empty — Chromium reads a file input's file
// at submit time, from a path we chose, and that read is the one thing no fake can stand in for.
//
// It is also the test that says the design works at all. The file is one the AGENT WROTE, passed as
// contents rather than as a path, so what arrives at the other end proves that carrying the bytes
// through the tool call is a real way to attach a document — not merely a safer-sounding one.
func TestAnAttachedFileReachesTheServer(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admittingWritable(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/attach"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	reading, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	upload := driver.Act
	attached, err := upload(ctx, session.ID, browser.Action{
		Kind:     browser.ActionUpload,
		Ref:      refFor(t, reading, "Document"),
		Filename: "relatorio.txt",
		Text:     "linha um\nlinha dois\n",
	})
	if err != nil {
		t.Fatalf("upload: %v", err)
	}
	if attached.Outcome != browser.OutcomeDone {
		t.Fatalf("the upload was refused: %+v", attached.Refusal)
	}
	// Attaching sends nothing. If it did, the record would say so here and the assertion below would
	// be measuring the wrong act.
	if len(attached.Writes) != 0 {
		t.Fatalf("attaching a file was recorded as a submission: %+v", attached.Writes)
	}

	sent, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick, Ref: refFor(t, reading, "Send"),
	})
	if err != nil {
		t.Fatalf("submit: %v", err)
	}
	if sent.Outcome != browser.OutcomeDone {
		t.Fatalf("the submission was refused: %+v", sent.Refusal)
	}

	var sawName, sawBody bool
	for _, arrival := range site.arrivals(15 * time.Second) {
		switch {
		case arrival == "attach-name=relatorio.txt":
			sawName = true
		case arrival == "attach-body=linha um\nlinha dois\n":
			sawBody = true
		case arrival == "attach-no-file":
			t.Fatal("the form was submitted with no file in it: the attachment did not survive " +
				"between the act that made it and the act that sent it")
		case arrival == "attach-not-multipart":
			t.Fatal("the submission was not multipart, so nothing could have carried a file")
		}
	}
	if !sawName {
		t.Fatal("no attachment reached the server under the name it was given")
	}
	if !sawBody {
		t.Fatal("the attachment arrived without the contents the agent wrote")
	}

	// And the record says a file went. This is the half a person reads afterwards, and "a comment
	// was posted" and "a document was posted" must not be the same row.
	if len(sent.Writes) != 1 {
		t.Fatalf("writes = %+v, want the one submission", sent.Writes)
	}
	if got := sent.Writes[0].Files; len(got) != 1 || got[0] != "relatorio.txt" {
		t.Fatalf("the record does not say a file left: %+v", sent.Writes[0])
	}
	// Names, never contents. The record is what makes an agent's writing supervisable; it must not
	// become where everything an agent ever sent comes to rest.
	for _, field := range sent.Writes[0].Fields {
		if strings.Contains(field, "linha um") {
			t.Fatalf("the record kept what the file said: %+v", sent.Writes[0])
		}
	}
}

// TestAnAttachmentGoesNowhereWithoutAWriteGrant.
//
// The control that says upload needed no fence rule of its own. Attaching is allowed — it touches
// only the page — and the SUBMISSION is judged exactly as any other is: this profile may read the
// site and was granted nothing to write to it, so the form does not leave and the server never sees
// a byte of the file.
//
// Without this the suite above would pass against a design where attaching a file quietly bypassed
// the write rule, which is precisely the mistake a new verb makes.
func TestAnAttachmentGoesNowhereWithoutAWriteGrant(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/attach"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	reading, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	attached, err := driver.Act(ctx, session.ID, browser.Action{
		Kind:     browser.ActionUpload,
		Ref:      refFor(t, reading, "Document"),
		Filename: "segredo.txt",
		Text:     "nao devia sair",
	})
	if err != nil {
		t.Fatalf("upload: %v", err)
	}
	if attached.Outcome != browser.OutcomeDone {
		t.Fatalf("attaching was refused on a page the profile may read: %+v", attached.Refusal)
	}

	if _, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick, Ref: refFor(t, reading, "Send"),
	}); err != nil {
		t.Fatalf("submit: %v", err)
	}

	for _, arrival := range site.arrivals(8 * time.Second) {
		if strings.HasPrefix(arrival, "attach-") {
			t.Fatalf("a file left for an origin nobody granted writing to: %s", arrival)
		}
		if arrival == "POST /attached" {
			t.Fatal("the submission reached the server without a write grant")
		}
	}
}
