package adapter

import (
	"context"
	"errors"
	"strconv"
	"strings"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"
	"nucleoschannelkit/render"

	"nucleostelegram/format"
	"nucleostelegram/telegram"
)

// execEdit edits the first message of the target with the new text. Only the first message is ever
// edited: when the new text renders to a different number of pieces than the target has messages,
// the edit cannot be mapped one to one and the core falls back to a fresh send. Nothing is deleted.
func (a *Adapter) execEdit(ctx context.Context, e contract.Edit) ([]string, error) {
	dest, ok := parseDestination(e.Conversation)
	if !ok || len(e.TargetPlatformRefs) == 0 {
		return nil, kit.Permanent("invalid_action")
	}
	messageID, err := strconv.ParseInt(e.TargetPlatformRefs[0], 10, 64)
	if err != nil {
		return nil, kit.Permanent("invalid_action")
	}
	buttons, err := toButtons(e.Buttons)
	if err != nil {
		return nil, kit.Permanent("invalid_action")
	}

	renderer := NewRenderer()
	var pieces []render.Piece
	for _, p := range e.Parts {
		if p.Type != contract.OutText {
			return nil, kit.Permanent("invalid_action")
		}
		rp, err := render.Render(renderer, p.Text, maxText)
		if err != nil {
			return nil, kit.Permanent("invalid_action")
		}
		pieces = append(pieces, rp...)
	}
	if len(pieces) != len(e.TargetPlatformRefs) {
		return nil, kit.Permanent("edit_unsupported_shape")
	}

	piece := pieces[0]
	var markup [][]telegram.Button
	if len(pieces) == 1 {
		markup = buttons
	}
	parseMode := ""
	if piece.Native {
		parseMode = "HTML"
	}
	err = a.bot.EditMessageText(ctx, dest.ChatID, messageID, piece.Text, parseMode, markup)
	if err != nil && piece.Native && IsParseEntities(err) {
		err = a.bot.EditMessageText(ctx, dest.ChatID, messageID, format.StripTags(piece.Text), "", markup)
	}
	if err != nil && !isNotModified(err) {
		return nil, Classify(err)
	}
	return e.TargetPlatformRefs, nil
}

// execAnswer answers a callback query. The ack carries no refs: nothing was sent.
func (a *Adapter) execAnswer(ctx context.Context, an contract.AnswerAction) ([]string, error) {
	err := a.bot.AnswerCallbackQueryCtx(ctx, an.ActionRef, an.Text)
	if err != nil && !isQueryTooOld(err) {
		return nil, Classify(err)
	}
	return nil, nil
}

// Typing shows the typing indicator. It is best-effort: errors go back to the kit, which only logs.
func (a *Adapter) Typing(ctx context.Context, conversation string) error {
	dest, ok := parseDestination(conversation)
	if !ok {
		return kit.Permanent("invalid_action")
	}
	return a.bot.SendChatAction(ctx, dest, "typing")
}

// isNotModified reports whether Telegram refused an edit because the message already holds that content.
func isNotModified(err error) bool {
	var apiErr *telegram.APIError
	return errors.As(err, &apiErr) && apiErr.StatusCode == 400 &&
		strings.Contains(strings.ToLower(apiErr.Description), "message is not modified")
}

// isQueryTooOld reports whether the callback query already expired: the spinner is gone, so there is
// nothing left to answer.
func isQueryTooOld(err error) bool {
	var apiErr *telegram.APIError
	return errors.As(err, &apiErr) && apiErr.StatusCode == 400 &&
		strings.Contains(strings.ToLower(apiErr.Description), "query is too old")
}
