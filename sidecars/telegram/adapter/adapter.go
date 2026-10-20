package adapter

import (
	"context"
	"io"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// SendBot is the part of the Telegram client the adapter sends through; tests fake it.
type SendBot interface {
	SendHTMLEx(ctx context.Context, p telegram.SendParams) (int64, error)
	EditMessageText(ctx context.Context, chatID, messageID int64, text, parseMode string, buttons [][]telegram.Button) error
	SendChatAction(ctx context.Context, to telegram.Destination, action string) error
	AnswerCallbackQueryCtx(ctx context.Context, callbackID, text string) error
}

// MediaDownloader fetches the bytes of a media id from the core; *kit.Client satisfies it.
type MediaDownloader interface {
	DownloadMedia(ctx context.Context, id string, max int64) (io.ReadCloser, error)
}

// Adapter executes outbox actions on Telegram.
type Adapter struct {
	bot   SendBot
	media MediaDownloader
}

var _ kit.Adapter = (*Adapter)(nil)

// New builds the adapter. media is used by media parts (image, file, audio).
func New(bot SendBot, media MediaDownloader) *Adapter {
	return &Adapter{bot: bot, media: media}
}

// Capabilities is what the adapter declares in its hello.
func (a *Adapter) Capabilities() contract.Capabilities { return Capabilities() }

// Execute dispatches one action by type.
func (a *Adapter) Execute(ctx context.Context, act contract.Action) ([]string, error) {
	switch v := act.(type) {
	case contract.Send:
		return a.execSend(ctx, v)
	case contract.Edit:
		return a.execEdit(ctx, v)
	case contract.AnswerAction:
		return a.execAnswer(ctx, v)
	}
	return nil, kit.Permanent("invalid_action")
}

// ---- STUB: B46 deletes execMedia from this block. ----

// execMedia sends one image, file or audio part and returns its message ref.
func (a *Adapter) execMedia(context.Context, telegram.Destination, contract.OutPart, sendOpts) (string, error) {
	return "", kit.Permanent("unsupported_media")
}

// ---- END B46 STUB ----
