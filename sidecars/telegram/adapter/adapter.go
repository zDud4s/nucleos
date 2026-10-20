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

// ---- STUBS: B45 deletes execEdit, execAnswer and Typing from this block. ----

func (a *Adapter) execEdit(context.Context, contract.Edit) ([]string, error) {
	return nil, kit.Permanent("unsupported_edit")
}

func (a *Adapter) execAnswer(context.Context, contract.AnswerAction) ([]string, error) {
	return nil, kit.Permanent("unsupported_answer")
}

// Typing is a stub until B45.
func (a *Adapter) Typing(context.Context, string) error {
	return kit.Permanent("unsupported_typing")
}

// ---- END B45 STUBS ----

// ---- STUB: B46 deletes execMedia from this block. ----

// execMedia sends one image, file or audio part and returns its message ref.
func (a *Adapter) execMedia(context.Context, telegram.Destination, contract.OutPart, sendOpts) (string, error) {
	return "", kit.Permanent("unsupported_media")
}

// ---- END B46 STUB ----
