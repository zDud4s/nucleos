package extract

import (
	"strings"
	"testing"
	"time"
)

func at(t string) time.Time {
	parsed, err := time.Parse(time.RFC3339, t)
	if err != nil {
		panic(err)
	}
	return parsed
}

// `received_at` comes from the server's INTERNALDATE, never the `Date:` header — the sender writes
// that one, and it governs the núcleo's backfill cutoff and retention. A hostile value there would
// decide whether their own message is triaged or filed.
func TestReceivedAtIgnoresTheSenderDateHeader(t *testing.T) {
	raw := []byte("From: Ana <ana@company.com>\r\n" +
		"Date: Tue, 01 Jan 2030 00:00:00 +0000\r\n" +
		"Subject: hello\r\n\r\nbody\r\n")

	message, err := Message(raw, 7, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.ReceivedAt != "2026-07-28T10:00:00Z" {
		t.Fatalf("received_at = %q, want the INTERNALDATE", message.ReceivedAt)
	}
}

func TestSenderNameAndAddressAreSeparated(t *testing.T) {
	raw := []byte("From: \"Ana Silva\" <ana@company.com>\r\nSubject: hi\r\n\r\nbody\r\n")
	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.FromAddr != "ana@company.com" {
		t.Fatalf("from_addr = %q", message.FromAddr)
	}
	if message.FromName != "Ana Silva" {
		t.Fatalf("from_name = %q", message.FromName)
	}
}

func TestPlainTextIsPreferredOverHTML(t *testing.T) {
	raw := []byte("From: a@b\r\nSubject: multi\r\n" +
		"Content-Type: multipart/alternative; boundary=X\r\n\r\n" +
		"--X\r\nContent-Type: text/plain\r\n\r\nthe plain one\r\n" +
		"--X\r\nContent-Type: text/html\r\n\r\n<p>the html one</p>\r\n--X--\r\n")

	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(message.BodyText, "the plain one") {
		t.Fatalf("body = %q, want the text/plain part", message.BodyText)
	}
	if strings.Contains(message.BodyText, "the html one") {
		t.Fatalf("body = %q, must not carry the html part too", message.BodyText)
	}
}

func TestHTMLOnlyFallsBackToStrippedText(t *testing.T) {
	raw := []byte("From: a@b\r\nSubject: html\r\nContent-Type: text/html\r\n\r\n" +
		"<html><body><p>hello&nbsp;there</p></body></html>\r\n")

	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.BodyText != "hello there" {
		t.Fatalf("body = %q, want the stripped text", message.BodyText)
	}
}

func TestAttachmentsAreFlagged(t *testing.T) {
	raw := []byte("From: a@b\r\nSubject: with file\r\n" +
		"Content-Type: multipart/mixed; boundary=X\r\n\r\n" +
		"--X\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n" +
		"--X\r\nContent-Type: application/pdf\r\n" +
		"Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\r\n%PDF-1.4\r\n--X--\r\n")

	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if !message.HasAttachments {
		t.Fatal("has_attachments should be true")
	}
	if strings.Contains(message.BodyText, "%PDF") {
		t.Fatalf("the attachment must not land in the body: %q", message.BodyText)
	}
}

func TestOnlyTheNoiseGateHeadersAreForwarded(t *testing.T) {
	raw := []byte("From: a@b\r\nSubject: bulk\r\n" +
		"List-Unsubscribe: <mailto:x@y>\r\n" +
		"Precedence: bulk\r\n" +
		"X-Some-Internal-Header: secret\r\n\r\nbody\r\n")

	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.Headers["list-unsubscribe"] == "" || message.Headers["precedence"] == "" {
		t.Fatalf("headers = %+v, want the gate's headers", message.Headers)
	}
	if len(message.Headers) != 2 {
		t.Fatalf("headers = %+v, want only what the gate reads", message.Headers)
	}
}

func TestABodyIsTruncatedWithoutSplittingACharacter(t *testing.T) {
	long := strings.Repeat("é", MaxBodyBytes)
	truncated := Truncate(long)
	if len(truncated) > MaxBodyBytes {
		t.Fatalf("truncated to %d bytes, want at most %d", len(truncated), MaxBodyBytes)
	}
	if !strings.HasPrefix(long, truncated) {
		t.Fatal("truncation must be a prefix")
	}
	for _, r := range truncated {
		if r == '�' {
			t.Fatal("truncation split a character")
		}
	}
}

// Entities are decoded once, not twice. Tags are stripped BEFORE decoding, so a decoder that runs
// `&amp;` -> `&` and then `&lt;` -> `<` lets a sender smuggle markup past the stripper by encoding
// it twice: `&amp;lt;` survives the tag pass untouched and only then turns into `<`. Correct
// single-pass decoding yields the literal `&lt;` the sender actually wrote. This text is triage
// input taken from a stranger, so it must mean what it said.
func TestEntitiesAreDecodedOnce(t *testing.T) {
	for _, c := range []struct{ in, want string }{
		{"&amp;lt;script&amp;gt;", "&lt;script&gt;"},
		{"&amp;amp;", "&amp;"},
		{"a &amp; b", "a & b"},
		{"&lt;b&gt;", "<b>"},
		{"hello&nbsp;there", "hello there"},
	} {
		if got := StripHTML(c.in); got != c.want {
			t.Errorf("StripHTML(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

func TestASubjectIsMimeDecoded(t *testing.T) {
	raw := []byte("From: a@b\r\nSubject: =?utf-8?B?cmV1bmnDo28=?=\r\n\r\nbody\r\n")
	message, err := Message(raw, 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.Subject != "reunião" {
		t.Fatalf("subject = %q", message.Subject)
	}
}
