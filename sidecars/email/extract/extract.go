// Package extract turns a raw RFC 5322 message into the envelope the núcleo stores.
//
// Pure on purpose: every rule that decides what the núcleo sees is exercised from bytes, with no
// server, no network and no credentials. The IMAP side stays thin enough to be uninteresting.
package extract

import (
	"encoding/base64"
	"html"
	"io"
	"mime"
	"mime/multipart"
	"net/mail"
	"regexp"
	"strings"
	"time"

	"nucleosemail/daemon"
)

// MaxBodyBytes mirrors the núcleo's cap. Truncating here as well keeps a hostile message from
// making the delivery payload enormous before the núcleo ever sees it.
const MaxBodyBytes = 32 * 1024

// HeadersOfInterest are the ones the núcleo's noise gate reads (spec §4.2). Only these are
// forwarded: the rest of a message's headers are not the sidecar's to relay, and a smaller payload
// is a smaller surface.
var HeadersOfInterest = []string{"list-unsubscribe", "precedence", "auto-submitted"}

var tagPattern = regexp.MustCompile(`(?s)<[^>]*>`)

// Message builds one delivery record.
//
// `internalDate` is the server's IMAP INTERNALDATE and is used as `received_at` — deliberately NOT
// the `Date:` header, which the sender writes and could therefore use to govern the núcleo's
// backfill cutoff and retention with a value of their choosing (spec §3.3).
func Message(raw []byte, uid uint32, internalDate time.Time) (daemon.Message, error) {
	parsed, err := mail.ReadMessage(strings.NewReader(string(raw)))
	if err != nil {
		return daemon.Message{}, err
	}

	fromAddr, fromName := sender(parsed.Header)
	body, attachments := bodyAndAttachments(parsed)

	headers := map[string]string{}
	for _, name := range HeadersOfInterest {
		if value := parsed.Header.Get(name); value != "" {
			// Lowercased keys are the contract with the núcleo's gate, which is nevertheless
			// case-insensitive so a slip here cannot silently switch the gate off.
			headers[name] = value
		}
	}

	return daemon.Message{
		MessageID:      strings.TrimSpace(parsed.Header.Get("Message-ID")),
		UID:            uid,
		FromAddr:       fromAddr,
		FromName:       fromName,
		Subject:        decodeHeader(parsed.Header.Get("Subject")),
		ReceivedAt:     internalDate.UTC().Format(time.RFC3339),
		BodyText:       Truncate(body),
		HasAttachments: len(attachments) > 0,
		Attachments:    attachments,
		Headers:        headers,
	}, nil
}

// Truncate caps a body without splitting a UTF-8 character.
func Truncate(body string) string {
	if len(body) <= MaxBodyBytes {
		return body
	}
	end := MaxBodyBytes
	for end > 0 && !isBoundary(body, end) {
		end--
	}
	return body[:end]
}

func isBoundary(s string, i int) bool {
	if i == 0 || i == len(s) {
		return true
	}
	return s[i]&0xC0 != 0x80
}

func decodeHeader(value string) string {
	decoded, err := new(mime.WordDecoder).DecodeHeader(value)
	if err != nil {
		// An undecodable header is still worth showing raw: it is a subject line, not a decision.
		return value
	}
	return decoded
}

func sender(header mail.Header) (addr string, name string) {
	parsed, err := mail.ParseAddress(header.Get("From"))
	if err != nil || parsed == nil {
		// Better a raw From than none: the address is what the noise gate matches on, and a
		// message whose From cannot be parsed is exactly the kind a person may still want to see.
		return strings.TrimSpace(header.Get("From")), ""
	}
	return parsed.Address, decodeHeader(parsed.Name)
}

// MaxMIMEDepth bounds how far the walk descends. Real mail nests three or four levels; anything
// deeper is a message built to make a parser recurse, not to be read.
const MaxMIMEDepth = 10

// collector accumulates one message's text and attachment descriptions across the MIME tree.
type collector struct {
	// Not named `html`: that is an imported package here, and shadowing it would make
	// `html.UnescapeString` fail to compile for a non-obvious reason.
	plain       string
	htmlPart    string
	attachments []daemon.Attachment
}

// bodyAndAttachments prefers `text/plain`, falls back to stripped HTML, and describes every
// attachment. The HTML fallback is deliberately naive: this text is triage input, not a rendering,
// and a real HTML parser would be a dependency earning nothing.
//
// It DESCENDS the MIME tree, and that is not a refinement — it is the difference between reading a
// message and not. Gmail wraps any message carrying an attachment in a `multipart/mixed` whose
// first part is the `multipart/alternative` that holds the actual text, and does the same with
// `multipart/related` for inline images. A walk that reads only the top level finds no `text/*`
// there at all and returns nothing: in the first real mailbox this pillar ever read, a third of
// the messages arrived with an empty body and were triaged on subject and sender alone.
func bodyAndAttachments(msg *mail.Message) (string, []daemon.Attachment) {
	mediaType, params, err := mime.ParseMediaType(msg.Header.Get("Content-Type"))
	if err != nil {
		body, _ := io.ReadAll(io.LimitReader(msg.Body, MaxBodyBytes*4))
		return string(body), nil
	}

	if !strings.HasPrefix(mediaType, "multipart/") {
		body, _ := io.ReadAll(io.LimitReader(msg.Body, MaxBodyBytes*4))
		if mediaType == "text/html" {
			return StripHTML(string(body)), nil
		}
		return string(body), nil
	}

	var found collector
	found.walk(msg.Body, params["boundary"], 0)

	if found.plain != "" {
		return found.plain, found.attachments
	}
	return StripHTML(found.htmlPart), found.attachments
}

// walk reads one multipart level, descending into nested multiparts and stopping at MaxMIMEDepth.
func (c *collector) walk(body io.Reader, boundary string, depth int) {
	if depth >= MaxMIMEDepth || boundary == "" {
		return
	}
	reader := multipart.NewReader(body, boundary)
	for {
		part, err := reader.NextPart()
		if err != nil {
			break
		}
		partType, partParams, _ := mime.ParseMediaType(part.Header.Get("Content-Type"))
		if strings.HasPrefix(partType, "multipart/") {
			c.walk(part, partParams["boundary"], depth+1)
			part.Close()
			continue
		}

		disposition, dispParams, _ := mime.ParseMediaType(part.Header.Get("Content-Disposition"))
		if disposition == "attachment" {
			c.attachments = append(c.attachments, daemon.Attachment{
				Position:  len(c.attachments),
				Filename:  attachmentName(dispParams, partParams),
				MimeType:  partType,
				SizeBytes: partSize(part),
			})
			part.Close()
			continue
		}

		content, _ := io.ReadAll(io.LimitReader(part, MaxBodyBytes*4))
		switch {
		case strings.HasPrefix(partType, "text/plain") && c.plain == "":
			c.plain = string(content)
		case strings.HasPrefix(partType, "text/html") && c.htmlPart == "":
			c.htmlPart = string(content)
		}
		part.Close()
	}
}

// attachmentName reads the filename from `Content-Disposition`, falling back to `Content-Type`'s
// `name` for the older senders that only set that one. Empty is a legal answer: an attachment with
// no name is still an attachment, and inventing one would be inventing information.
func attachmentName(dispParams, typeParams map[string]string) string {
	if name := dispParams["filename"]; name != "" {
		return decodeHeader(name)
	}
	if name := typeParams["name"]; name != "" {
		return decodeHeader(name)
	}
	return ""
}

// partSize reports the DECODED size, which is the number a person recognises from their own file
// system. `multipart` decodes quoted-printable transparently and hides the header when it does,
// but leaves base64 alone — and base64 is 4/3 of the real thing, so reporting it raw would
// overstate the size of essentially every attachment Gmail sends.
func partSize(part *multipart.Part) int64 {
	var reader io.Reader = part
	if strings.EqualFold(part.Header.Get("Content-Transfer-Encoding"), "base64") {
		reader = base64.NewDecoder(base64.StdEncoding, part)
	}
	size, _ := io.Copy(io.Discard, reader)
	return size
}

// StripHTML reduces markup to the text a triage prompt needs.
//
// Decoding is a SINGLE pass, which sequential `strings.ReplaceAll` calls are not: replacing
// `&amp;` before `&lt;` decodes `&amp;lt;` twice and yields `<`, so a sender could smuggle markup
// past the tag pass above (which runs first, and sees no tag in `&amp;lt;`) by encoding it twice.
// `html.UnescapeString` resolves each entity once and covers the numeric forms the hand-rolled list
// missed. `&nbsp;` becomes U+00A0, which `strings.Fields` counts as space, so it still collapses.
func StripHTML(markup string) string {
	text := tagPattern.ReplaceAllString(markup, " ")
	text = html.UnescapeString(text)
	return strings.TrimSpace(strings.Join(strings.Fields(text), " "))
}
