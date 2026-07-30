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

func TestSentMessageForwardsRecipients(t *testing.T) {
	raw := []byte("From: Ana <ana@example.test>\r\n" +
		"To: Maria <maria@example.test>, oncall@example.test\r\n" +
		"Subject: hello\r\n\r\nbody\r\n")

	message, err := SentMessage(raw, 7, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	recipients := message.Headers["to"]
	if !strings.Contains(recipients, "maria@example.test") ||
		!strings.Contains(recipients, "oncall@example.test") {
		t.Fatalf("to header = %q, want both recipients", recipients)
	}
}

func TestInboxMessageKeepsRecipientsOut(t *testing.T) {
	raw := []byte("From: Ana <ana@example.test>\r\n" +
		"To: Maria <maria@example.test>, oncall@example.test\r\n" +
		"Subject: hello\r\n\r\nbody\r\n")

	message, err := Message(raw, 7, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := message.Headers["to"]; ok {
		t.Fatalf("headers = %+v, inbox must not forward recipients", message.Headers)
	}
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

// gmailWithAttachment is the shape Gmail actually sends when a message carries a file: the text is
// one level down, inside a `multipart/alternative` that is itself the first part of a
// `multipart/mixed`. Reading only the top level finds no `text/*` and returns nothing.
const gmailWithAttachment = "From: Ana <ana@company.com>\r\n" +
	"Subject: cotacao\r\n" +
	"Content-Type: multipart/mixed; boundary=\"OUTER\"\r\n" +
	"\r\n" +
	"--OUTER\r\n" +
	"Content-Type: multipart/alternative; boundary=\"INNER\"\r\n" +
	"\r\n" +
	"--INNER\r\n" +
	"Content-Type: text/plain; charset=UTF-8\r\n" +
	"\r\n" +
	"o texto que interessa\r\n" +
	"--INNER\r\n" +
	"Content-Type: text/html; charset=UTF-8\r\n" +
	"\r\n" +
	"<p>o texto que interessa</p>\r\n" +
	"--INNER--\r\n" +
	"--OUTER\r\n" +
	"Content-Type: application/pdf; name=\"cotacao.pdf\"\r\n" +
	"Content-Disposition: attachment; filename=\"cotacao.pdf\"\r\n" +
	"Content-Transfer-Encoding: base64\r\n" +
	"\r\n" +
	"SGVsbG8sIHdvcmxkIQ==\r\n" +
	"--OUTER--\r\n"

// The regression that mattered: every message with an attachment in the first real mailbox came
// through with an empty body and was triaged on subject and sender alone.
func TestBodyIsFoundInsideNestedMultipart(t *testing.T) {
	message, err := Message([]byte(gmailWithAttachment), 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.BodyText != "o texto que interessa" {
		t.Fatalf("body = %q, want the text from the nested text/plain part", message.BodyText)
	}
}

func TestAttachmentIsDescribedNotDelivered(t *testing.T) {
	message, err := Message([]byte(gmailWithAttachment), 1, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if !message.HasAttachments || len(message.Attachments) != 1 {
		t.Fatalf("got %d attachments (flag %v), want exactly 1",
			len(message.Attachments), message.HasAttachments)
	}
	got := message.Attachments[0]
	if got.Filename != "cotacao.pdf" || got.MimeType != "application/pdf" {
		t.Errorf("described as %q/%q, want cotacao.pdf/application/pdf", got.Filename, got.MimeType)
	}
	if got.Position != 0 {
		t.Errorf("position = %d, want 0", got.Position)
	}
	// "Hello, world!" — the DECODED length. Reporting base64's 20 bytes would overstate every
	// attachment Gmail sends by a third.
	if got.SizeBytes != 13 {
		t.Errorf("size = %d, want the decoded 13", got.SizeBytes)
	}
}

// `multipart/related` wrapping the text is how mail with inline images arrives, and it emptied
// bodies the same way — with `has_attachments` false, so nothing hinted at why.
func TestBodyIsFoundInsideMultipartRelated(t *testing.T) {
	raw := "From: Ana <ana@company.com>\r\n" +
		"Content-Type: multipart/related; boundary=\"REL\"\r\n" +
		"\r\n" +
		"--REL\r\n" +
		"Content-Type: multipart/alternative; boundary=\"ALT\"\r\n" +
		"\r\n" +
		"--ALT\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"corpo real\r\n" +
		"--ALT--\r\n" +
		"--REL\r\n" +
		"Content-Type: image/png\r\n" +
		"Content-Disposition: inline; filename=\"logo.png\"\r\n" +
		"\r\n" +
		"binary\r\n" +
		"--REL--\r\n"

	message, err := Message([]byte(raw), 2, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if message.BodyText != "corpo real" {
		t.Fatalf("body = %q, want the nested text", message.BodyText)
	}
	// An inline image is part of the message being shown, not a file someone sent. Listing it
	// would make almost every newsletter claim attachments.
	if message.HasAttachments {
		t.Error("an inline image was counted as an attachment")
	}
}

// Older senders put the name only on Content-Type.
func TestAttachmentNameFallsBackToContentType(t *testing.T) {
	raw := "From: Ana <ana@company.com>\r\n" +
		"Content-Type: multipart/mixed; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"texto\r\n" +
		"--B\r\n" +
		"Content-Type: application/octet-stream; name=\"antigo.doc\"\r\n" +
		"Content-Disposition: attachment\r\n" +
		"\r\n" +
		"conteudo\r\n" +
		"--B--\r\n"

	message, err := Message([]byte(raw), 3, at("2026-07-28T10:00:00Z"))
	if err != nil {
		t.Fatal(err)
	}
	if len(message.Attachments) != 1 || message.Attachments[0].Filename != "antigo.doc" {
		t.Fatalf("attachments = %+v, want one named antigo.doc", message.Attachments)
	}
}

// A message nested past MaxMIMEDepth stops the walk instead of recursing on someone else's terms.
func TestDeeplyNestedMessageTerminates(t *testing.T) {
	var raw strings.Builder
	raw.WriteString("From: Ana <ana@company.com>\r\n")
	raw.WriteString("Content-Type: multipart/mixed; boundary=\"B0\"\r\n\r\n")
	depth := MaxMIMEDepth + 5
	for level := 0; level < depth; level++ {
		raw.WriteString("--B" + string(rune('0'+level%10)) + "\r\n")
		raw.WriteString("Content-Type: multipart/mixed; boundary=\"B" +
			string(rune('0'+(level+1)%10)) + "\"\r\n\r\n")
	}
	if _, err := Message([]byte(raw.String()), 4, at("2026-07-28T10:00:00Z")); err != nil {
		t.Fatalf("a deeply nested message should parse to something, got %v", err)
	}
}

// Two attachments, so the pairing between a description and its bytes is actually exercised: with
// one of each, a function that returned the wrong content would still look right.
func TestAllAttachmentsPairsEachDescriptionWithItsOwnBytes(t *testing.T) {
	raw := "From: Ana <ana@company.com>\r\n" +
		"Content-Type: multipart/mixed; boundary=\"B\"\r\n" +
		"\r\n" +
		"--B\r\n" +
		"Content-Type: multipart/alternative; boundary=\"A\"\r\n" +
		"\r\n" +
		"--A\r\n" +
		"Content-Type: text/plain\r\n" +
		"\r\n" +
		"texto\r\n" +
		"--A--\r\n" +
		"--B\r\n" +
		"Content-Type: application/pdf\r\n" +
		"Content-Disposition: attachment; filename=\"primeiro.pdf\"\r\n" +
		"\r\n" +
		"conteudo do primeiro\r\n" +
		"--B\r\n" +
		"Content-Type: application/pdf\r\n" +
		"Content-Disposition: attachment; filename=\"segundo.pdf\"\r\n" +
		"Content-Transfer-Encoding: base64\r\n" +
		"\r\n" +
		"c2VndW5kbw==\r\n" +
		"--B--\r\n"

	described, contents, err := AllAttachments([]byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	if len(described) != 2 || len(contents) != 2 {
		t.Fatalf("got %d described and %d contents, want 2 of each", len(described), len(contents))
	}
	if described[0].Filename != "primeiro.pdf" || described[1].Filename != "segundo.pdf" {
		t.Fatalf("described %q and %q, want primeiro/segundo",
			described[0].Filename, described[1].Filename)
	}
	if string(contents[0]) != "conteudo do primeiro" {
		t.Errorf("position 0 carried %q", contents[0])
	}
	// Decoded, not the base64 that arrived.
	if string(contents[1]) != "segundo" {
		t.Errorf("position 1 carried %q, want the decoded bytes", contents[1])
	}
	// And the size reported matches the bytes handed over, or a caller sizing a list would lie.
	if described[1].SizeBytes != int64(len(contents[1])) {
		t.Errorf("size %d does not match the %d bytes returned",
			described[1].SizeBytes, len(contents[1]))
	}
}

// A message with no multipart structure carries no attachments, and "none" is the truthful answer
// rather than an error the caller has to special-case.
func TestAllAttachmentsOnAPlainMessageIsEmpty(t *testing.T) {
	raw := "From: Ana <ana@company.com>\r\nContent-Type: text/plain\r\n\r\nso texto\r\n"
	described, contents, err := AllAttachments([]byte(raw))
	if err != nil || len(described) != 0 || len(contents) != 0 {
		t.Fatalf("got %d/%d err=%v, want nothing and no error", len(described), len(contents), err)
	}
}
