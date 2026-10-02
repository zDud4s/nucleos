// §spec email-pillar

// Package extract turns a raw RFC 5322 message into the envelope the núcleo stores.
//
// Pure on purpose: every rule that decides what the núcleo sees is exercised from bytes, with no
// server, no network and no credentials. The IMAP side stays thin enough to be uninteresting.
package extract

import (
	"encoding/base64"
	"errors"
	"html"
	"io"
	"mime"
	"mime/multipart"
	"mime/quotedprintable"
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

// SentMessage builds one delivery record for a message the USER wrote.
//
// The sent folder is read for a different reason than the inbox: there, the interesting party is
// the sender; here it is the recipient, because writing to someone is what makes them a known
// correspondent. Recipients are therefore forwarded only from this path — an inbox message's
// `To:` line names people the user did not choose to tell us about, and the núcleo discards it
// anyway. The núcleo also discards an outbound body and stores no record of an outbound attachment,
// so putting either on the wire would move the user's own content across a process boundary only to
// be dropped. Not sending it is the smaller surface, and the threat model need not explain why
// sending it is harmless.
func SentMessage(raw []byte, uid uint32, internalDate time.Time) (daemon.Message, error) {
	message, err := Message(raw, uid, internalDate)
	if err != nil {
		return daemon.Message{}, err
	}

	parsed, err := mail.ReadMessage(strings.NewReader(string(raw)))
	if err != nil {
		return daemon.Message{}, err
	}
	if to := strings.TrimSpace(parsed.Header.Get("To")); to != "" {
		message.Headers["to"] = to
	}
	message.BodyText = ""
	message.Attachments = nil
	message.HasAttachments = false
	return message, nil
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
	decoder := &mime.WordDecoder{CharsetReader: func(charset string, input io.Reader) (io.Reader, error) {
		raw, err := io.ReadAll(input)
		if err != nil {
			return nil, err
		}
		return strings.NewReader(toUTF8(raw, charset)), nil
	}}
	decoded, err := decoder.DecodeHeader(value)
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

// MaxAttachmentBytes caps what a single attachment may weigh when its content is actually read.
// Gmail refuses to send more than this, so a part claiming more is not a file someone attached.
const MaxAttachmentBytes = 25 * 1024 * 1024

// ErrNoSuchAttachment means the message does not carry an attachment at that position — the stored
// description and the live message have diverged, which is the case a caller must not confuse with
// an empty file.
var ErrNoSuchAttachment = errors.New("no attachment at that position")

// What a walk should keep the bytes of, beside the descriptions it always builds.
const (
	wantNone  = -1
	wantEvery = -2
)

// collector accumulates one message's text and attachment descriptions across the MIME tree.
type collector struct {
	// Not named `html`: that is an imported package here, and shadowing it would make
	// `html.UnescapeString` fail to compile for a non-obvious reason.
	plain       string
	htmlPart    string
	attachments []daemon.Attachment
	// want is `wantNone` for the ordinary describe-only walk, `wantEvery` to keep everything, or a
	// single position. All three readings share ONE walk so the position that addresses an
	// attachment is derived the same way in each — a second traversal with its own idea of ordering
	// is how a description and its content start pointing at different files.
	want     int
	contents map[int][]byte
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
		return readText(msg.Body, msg.Header.Get("Content-Transfer-Encoding"), ""), nil
	}

	if !strings.HasPrefix(mediaType, "multipart/") {
		body := readText(msg.Body, msg.Header.Get("Content-Transfer-Encoding"), params["charset"])
		if mediaType == "text/html" {
			return StripHTML(body), nil
		}
		return body, nil
	}

	found := collector{want: wantNone}
	found.walk(msg.Body, params["boundary"], 0)

	if found.plain != "" {
		return found.plain, found.attachments
	}
	return StripHTML(found.htmlPart), found.attachments
}

// Attachment returns one attachment's description and its bytes, addressed by the same position
// the stored description carries.
//
// The message is re-read from the mailbox rather than kept: an attachment is a stranger's file, and
// the one place it is guaranteed to already exist is the mailbox it arrived in. Nothing is written
// to disk on the way through.
func Attachment(raw []byte, position int) (daemon.Attachment, []byte, error) {
	parsed, err := mail.ReadMessage(strings.NewReader(string(raw)))
	if err != nil {
		return daemon.Attachment{}, nil, err
	}
	mediaType, params, err := mime.ParseMediaType(parsed.Header.Get("Content-Type"))
	if err != nil || !strings.HasPrefix(mediaType, "multipart/") {
		return daemon.Attachment{}, nil, ErrNoSuchAttachment
	}

	found := collector{want: position}
	found.walk(parsed.Body, params["boundary"], 0)
	if position < 0 || position >= len(found.attachments) {
		return daemon.Attachment{}, nil, ErrNoSuchAttachment
	}
	described, content := found.attachments[position], found.contents[position]
	if described.SizeBytes > int64(len(content)) {
		return described, content, ErrAttachmentTruncated
	}
	return described, content, nil
}

// AllAttachments returns every attachment in one pass, described and with its bytes.
//
// The reason this exists rather than a loop over `Attachment` is arithmetic: each call fetches the
// WHOLE message from IMAP, so eight attachments meant eight downloads of the same eight files, and
// eight TLS handshakes. Reading them together is one fetch, and the total is bounded anyway — every
// attachment in a message weighs less than the message.
//
// An attachment past MaxAttachmentBytes comes back with its true `SizeBytes` and only the first
// MaxAttachmentBytes of content, so `SizeBytes > len(content)` marks the truncation for the caller.
func AllAttachments(raw []byte) ([]daemon.Attachment, map[int][]byte, error) {
	parsed, err := mail.ReadMessage(strings.NewReader(string(raw)))
	if err != nil {
		return nil, nil, err
	}
	mediaType, params, err := mime.ParseMediaType(parsed.Header.Get("Content-Type"))
	if err != nil || !strings.HasPrefix(mediaType, "multipart/") {
		// Not an error: a message with no multipart structure carries no attachments, and saying
		// "none" is the truthful answer to "give me all of them".
		return nil, map[int][]byte{}, nil
	}

	found := collector{want: wantEvery}
	found.walk(parsed.Body, params["boundary"], 0)
	if found.contents == nil {
		found.contents = map[int][]byte{}
	}
	return found.attachments, found.contents, nil
}

// readDecoded reads a part's real content, undoing base64 where multipart does not, and stops at
// MaxAttachmentBytes so one hostile part cannot be answered with unbounded memory.
//
// `size` is the part's TRUE decoded size: past the cap the rest is counted and discarded rather than
// kept, so a truncated read shows itself as `size > len(content)` instead of passing for a whole
// file.
func readDecoded(part *multipart.Part) (content []byte, size int64) {
	var reader io.Reader = part
	if strings.EqualFold(part.Header.Get("Content-Transfer-Encoding"), "base64") {
		reader = base64.NewDecoder(base64.StdEncoding, part)
	}
	content, _ = io.ReadAll(io.LimitReader(reader, MaxAttachmentBytes))
	size = int64(len(content))
	if size == MaxAttachmentBytes {
		rest, _ := io.Copy(io.Discard, reader)
		size += rest
	}
	return content, size
}

// ErrAttachmentTruncated means the attachment is larger than MaxAttachmentBytes, so the bytes
// returned beside it are only its beginning. A half file that looks whole is worse than none.
var ErrAttachmentTruncated = errors.New("attachment exceeds the size cap and was truncated")

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
			position := len(c.attachments)
			size := int64(0)
			if c.want == wantEvery || position == c.want {
				content, trueSize := readDecoded(part)
				if c.contents == nil {
					c.contents = map[int][]byte{}
				}
				c.contents[position] = content
				size = trueSize
			} else {
				size = partSize(part)
			}
			c.attachments = append(c.attachments, daemon.Attachment{
				Position:  position,
				Filename:  attachmentName(dispParams, partParams),
				MimeType:  partType,
				SizeBytes: size,
			})
			part.Close()
			continue
		}

		// `multipart` already undid quoted-printable (and dropped the header when it did); base64 and
		// the charset are still ours to handle.
		content := readText(part, part.Header.Get("Content-Transfer-Encoding"), partParams["charset"])
		switch {
		case strings.HasPrefix(partType, "text/plain") && c.plain == "":
			c.plain = content
		case strings.HasPrefix(partType, "text/html") && c.htmlPart == "":
			c.htmlPart = content
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

// readText reads a text part's bytes, undoes a Content-Transfer-Encoding still on the stream
// (base64, or quoted-printable on a top-level body that `multipart` never touched) and converts the
// declared charset to UTF-8. Bounded like every other body read.
func readText(source io.Reader, transferEncoding, charset string) string {
	switch strings.ToLower(strings.TrimSpace(transferEncoding)) {
	case "base64":
		source = base64.NewDecoder(base64.StdEncoding, source)
	case "quoted-printable":
		source = quotedprintable.NewReader(source)
	}
	raw, _ := io.ReadAll(io.LimitReader(source, MaxBodyBytes*4))
	return toUTF8(raw, charset)
}

// windows1252High maps 0x80-0x9F, the only range where windows-1252 differs from ISO-8859-1.
// Undefined slots stay as the C1 control, as browsers do.
var windows1252High = [32]rune{
	0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021,
	0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F,
	0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
	0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
}

// toUTF8 converts what the stdlib can without golang.org/x/text (not a dependency of this module):
// UTF-8 and US-ASCII pass through, ISO-8859-1/latin1 and windows-1252 are mapped natively. Any other
// charset is returned as-is, bytes unchanged, rather than guessed at; a dependency can be added if
// those turn up in real mail.
func toUTF8(raw []byte, charset string) string {
	switch strings.ToLower(strings.TrimSpace(charset)) {
	case "iso-8859-1", "iso8859-1", "latin1", "l1":
		out := make([]rune, len(raw))
		for i, b := range raw {
			out[i] = rune(b)
		}
		return string(out)
	case "windows-1252", "cp1252":
		out := make([]rune, len(raw))
		for i, b := range raw {
			if b >= 0x80 && b <= 0x9F {
				out[i] = windows1252High[b-0x80]
			} else {
				out[i] = rune(b)
			}
		}
		return string(out)
	}
	return string(raw)
}
