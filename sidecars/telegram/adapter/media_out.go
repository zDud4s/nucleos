package adapter

import (
	"context"
	"errors"
	"strconv"
	"strings"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// maxUploadBytes is the largest media the adapter fetches from the core to upload: Telegram's Bot API
// accepts up to 50 MB per upload, and a larger file is reported as media_too_large instead of truncated.
const maxUploadBytes = 50 << 20

// buttonsOnlyText is the text of the message that carries the buttons when an action has no text
// piece to carry them; Telegram rejects an empty message, so it needs some content.
const buttonsOnlyText = "Choose:"

// execMedia sends one image, file or audio part and returns the refs of the messages it produced:
// the media message, then a follow-up text message when the caption overflowed Telegram's limit.
// The returned error is already classified.
func (a *Adapter) execMedia(ctx context.Context, dest telegram.Destination, p contract.OutPart, o sendOpts) ([]string, error) {
	kind, ok := mediaKind(p.Type)
	if !ok {
		return nil, kit.Permanent("invalid_action")
	}
	if a.media == nil {
		return nil, kit.Permanent("media_unavailable")
	}
	body, err := a.media.DownloadMedia(ctx, p.Media, maxUploadBytes)
	if err != nil {
		return nil, classifyMediaDownload(err)
	}
	defer body.Close()

	sent, err := a.bot.SendMediaEx(ctx, kind, telegram.SendMediaParams{
		To: dest, Filename: p.Name, Mime: p.Mime, Caption: p.Caption, ReplyTo: o.replyTo, Silent: o.silent,
	}, body)
	if err != nil {
		if telegramRefusedSize(err) {
			return nil, kit.Permanent("media_too_large")
		}
		return nil, Classify(err)
	}
	refs := []string{strconv.FormatInt(sent.MessageID, 10)}
	if sent.CaptionOverflow != "" {
		id, err := a.bot.SendHTMLEx(ctx, telegram.SendParams{To: dest, Text: sent.CaptionOverflow, Silent: o.silent})
		if err != nil {
			return nil, Classify(err)
		}
		refs = append(refs, strconv.FormatInt(id, 10))
	}
	return refs, nil
}

// sendButtonsOnly sends the buttons in a message of their own, for an action with no text piece.
func (a *Adapter) sendButtonsOnly(ctx context.Context, dest telegram.Destination, buttons [][]telegram.Button, silent bool) (string, error) {
	id, err := a.bot.SendHTMLEx(ctx, telegram.SendParams{To: dest, Text: buttonsOnlyText, Buttons: buttons, Silent: silent})
	if err != nil {
		return "", Classify(err)
	}
	return strconv.FormatInt(id, 10), nil
}

func mediaKind(t contract.OutPartType) (telegram.MediaKind, bool) {
	switch t {
	case contract.OutImage:
		return telegram.MediaImage, true
	case contract.OutFile:
		return telegram.MediaFile, true
	case contract.OutAudio:
		return telegram.MediaAudio, true
	}
	return "", false
}

// classifyMediaDownload maps a failure to fetch the media from the core: a missing, forbidden or
// otherwise refused media will not appear on a retry; only transport trouble is worth retrying.
func classifyMediaDownload(err error) error {
	switch {
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
		return err
	case errors.Is(err, kit.ErrMediaTooLarge):
		return kit.Permanent("media_too_large")
	case errors.Is(err, kit.ErrMediaNotFound):
		return kit.Permanent("media_unavailable")
	case errors.Is(err, kit.ErrTransient):
		return kit.Retryable("media_download", 0)
	}
	return kit.Permanent("media_unavailable")
}

// telegramRefusedSize is true when the Bot API rejected the upload for its size.
func telegramRefusedSize(err error) bool {
	var apiErr *telegram.APIError
	if !errors.As(err, &apiErr) {
		return false
	}
	desc := strings.ToLower(apiErr.Description)
	return apiErr.StatusCode == 413 || strings.Contains(desc, "too big") ||
		strings.Contains(desc, "entity too large") || strings.Contains(desc, "file is too large")
}
