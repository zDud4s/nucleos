package adapter

import (
	"bytes"
	"context"
	"errors"
	"io"
	"regexp"
	"strconv"
	"strings"
	"unicode/utf8"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

const (
	// maxBotFileBytes is Telegram's own cap on a bot download; a larger file is refused before any
	// byte is fetched.
	maxBotFileBytes = 20 << 20
	// maxDownloadBytes mirrors the unexported cap of telegram.Client.DownloadFile, which truncates
	// silently with io.LimitReader. A body of exactly this size may therefore be a cut file and is
	// refused; keep it equal to the value in telegram/client.go.
	maxDownloadBytes = 20 << 20

	maxNameBytes = 256
)

// mimeShape is the contract's mime grammar; anything else is replaced by the default.
var mimeShape = regexp.MustCompile(`^[a-z0-9][a-z0-9.+-]*/[a-z0-9][a-z0-9.+-]*$`)

// FileBot is the slice of the Bot API client that fetches attachments.
type FileBot interface {
	GetFile(fileID string) (string, error)
	DownloadFile(filePath string) ([]byte, error)
}

// Uploader hands bytes to the core's media store; kit.Client.UploadMedia has this shape.
type Uploader interface {
	Upload(ctx context.Context, mime string, r io.Reader) (mediaID string, err error)
}

// AttachMedia downloads the attachments of msg from Telegram, uploads them to the core and appends the
// resulting audio, image and file parts to ev, before any unsupported part.
//
// A part that cannot be carried (Telegram will not serve it, it is over the size cap, the core says it
// is too large) becomes an unsupported part labelled voice, audio, photo or document, and the event
// goes on. Any other upload error is transient and is returned with ev untouched, so the caller does not
// advance the poll offset and the update is redelivered.
func AttachMedia(ctx context.Context, ev *contract.Event, msg *telegram.Message, bot FileBot, up Uploader) error {
	if ev == nil || ev.Message == nil || msg == nil {
		return nil
	}
	existing := ev.Message.Parts
	var added []contract.InPart

	add := func(label, fileID string, size int, mime string, build func(mediaID string) contract.InPart) error {
		part, err := fetchAndUpload(ctx, bot, up, fileID, size, mime, build)
		if err != nil {
			return err
		}
		if part == nil {
			p := unsupportedPart(label)
			part = &p
		}
		added = append(added, *part)
		return nil
	}

	if v := msg.Voice; v != nil {
		mime := cleanMime(v.MimeType, "audio/ogg")
		if err := add("voice", v.FileID, v.FileSize, mime, audioPart(mime, v.Duration)); err != nil {
			return err
		}
	}
	if a := msg.Audio; a != nil {
		mime := cleanMime(a.MimeType, "audio/mpeg")
		if err := add("audio", a.FileID, a.FileSize, mime, audioPart(mime, a.Duration)); err != nil {
			return err
		}
	}
	if len(msg.Photo) > 0 {
		p := largestPhoto(msg.Photo)
		build := func(mediaID string) contract.InPart {
			typ := contract.PartImage
			return withCaption(contract.InPart{Type: &typ, Media: &mediaID}, msg.Caption)
		}
		if err := add("photo", p.FileID, p.FileSize, "image/jpeg", build); err != nil {
			return err
		}
	}
	if d := msg.Document; d != nil {
		declared := cleanMime(d.MimeType, "")
		upMime := declared
		if upMime == "" {
			upMime = "application/octet-stream"
		}
		name := documentName(d.FileName, len(existing)+len(added))
		build := func(mediaID string) contract.InPart {
			typ := contract.PartFile
			part := contract.InPart{Type: &typ, Media: &mediaID, Name: &name}
			if declared != "" {
				part.Mime = &declared
			}
			return withCaption(part, msg.Caption)
		}
		if err := add("document", d.FileID, d.FileSize, upMime, build); err != nil {
			return err
		}
	}

	// Media goes after the text and before any unsupported marker, the order a reader expects.
	at := len(existing)
	for i, p := range existing {
		if *p.Type == contract.PartUnsupported {
			at = i
			break
		}
	}
	parts := make([]contract.InPart, 0, len(existing)+len(added)+1)
	if captionLost(msg, added) {
		parts = append(parts, textPart(msg.Caption))
	}
	parts = append(parts, existing[:at]...)
	parts = append(parts, added...)
	parts = append(parts, existing[at:]...)
	ev.Message.Parts = parts
	return nil
}

// captionLost reports that the caption of a photo or document, which Inbound left off the text parts
// because the media part was to carry it, has no home: every image and file failed to upload and
// became unsupported. The caption is the user's instruction and goes in as a text part instead.
func captionLost(msg *telegram.Message, added []contract.InPart) bool {
	if msg.Caption == "" || msg.Text != "" || (len(msg.Photo) == 0 && msg.Document == nil) {
		return false
	}
	for _, p := range added {
		if *p.Type == contract.PartImage || *p.Type == contract.PartFile {
			return false
		}
	}
	return true
}

// fetchAndUpload returns the part built for the uploaded media, or nil when the media cannot be
// carried. Only a transient upload failure is an error.
func fetchAndUpload(ctx context.Context, bot FileBot, up Uploader, fileID string, size int, mime string, build func(string) contract.InPart) (*contract.InPart, error) {
	if size > maxBotFileBytes {
		return nil, nil
	}
	path, err := bot.GetFile(fileID)
	if err != nil || path == "" {
		return nil, nil
	}
	data, err := bot.DownloadFile(path)
	if err != nil || len(data) >= maxDownloadBytes {
		return nil, nil
	}
	mediaID, err := up.Upload(ctx, mime, bytes.NewReader(data))
	if err != nil {
		if errors.Is(err, kit.ErrMediaTooLarge) {
			return nil, nil
		}
		return nil, err
	}
	part := build(mediaID)
	return &part, nil
}

func audioPart(mime string, seconds int) func(string) contract.InPart {
	return func(mediaID string) contract.InPart {
		typ := contract.PartAudio
		ms := uint64(0)
		if seconds > 0 {
			ms = uint64(seconds) * 1000
		}
		return contract.InPart{Type: &typ, Media: &mediaID, Mime: &mime, DurationMS: &ms}
	}
}

func withCaption(p contract.InPart, caption string) contract.InPart {
	if caption != "" {
		p.Caption = &caption
	}
	return p
}

// largestPhoto picks the biggest rendition Telegram offers (by pixels, then by bytes).
func largestPhoto(sizes []telegram.PhotoSize) telegram.PhotoSize {
	best := sizes[0]
	for _, s := range sizes[1:] {
		if s.Width*s.Height > best.Width*best.Height ||
			(s.Width*s.Height == best.Width*best.Height && s.FileSize > best.FileSize) {
			best = s
		}
	}
	return best
}

// cleanMime drops parameters ("; codecs=opus") and case, falling back to def when nothing valid is left.
func cleanMime(raw, def string) string {
	m := strings.ToLower(strings.TrimSpace(strings.SplitN(raw, ";", 2)[0]))
	if !mimeShape.MatchString(m) {
		return def
	}
	return m
}

// documentName is the name Telegram gave the file, or "file"+index when it gave none the contract
// would accept; the contract requires a name on every file part.
func documentName(name string, index int) string {
	if name == "" || len(name) > maxNameBytes || !utf8.ValidString(name) {
		return "file" + strconv.Itoa(index)
	}
	return name
}
