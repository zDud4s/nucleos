package adapter

import (
	"context"
	"log"
	"strconv"
	"strings"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/kit"
	"nucleoschannelkit/render"

	"nucleostelegram/format"
	"nucleostelegram/telegram"
)

// sendOpts are the per-message options shared by text and media sends.
type sendOpts struct {
	replyTo int64
	buttons [][]telegram.Button
	silent  bool
}

// parseDestination turns a conversation ref ("<chat>" or "<chat>:<thread>") into a Destination.
func parseDestination(ref string) (telegram.Destination, bool) {
	chatPart, threadPart, hasThread := strings.Cut(ref, ":")
	chat, err := strconv.ParseInt(chatPart, 10, 64)
	if err != nil {
		return telegram.Destination{}, false
	}
	d := telegram.Destination{ChatID: chat}
	if hasThread {
		thread, err := strconv.ParseInt(threadPart, 10, 64)
		if err != nil || thread == 0 {
			return telegram.Destination{}, false
		}
		d.ThreadID = thread
	}
	return d, true
}

// execSend sends the parts of a send action in order, one message per rendered text piece, and
// returns one message id per message sent. If a message fails, the ones already sent are only
// logged and the classified error is returned with no refs: the core redelivers the whole action,
// so a duplicate is accepted rather than acking a partial result.
func (a *Adapter) execSend(ctx context.Context, s contract.Send) ([]string, error) {
	dest, ok := parseDestination(s.Conversation)
	if !ok {
		return nil, kit.Permanent("invalid_action")
	}
	var replyTo int64
	if s.ReplyToPlatformRef != "" {
		id, err := strconv.ParseInt(s.ReplyToPlatformRef, 10, 64)
		if err != nil {
			return nil, kit.Permanent("invalid_action")
		}
		replyTo = id
	}
	buttons, err := toButtons(s.Buttons)
	if err != nil {
		return nil, kit.Permanent("invalid_action")
	}

	renderer := NewRenderer()
	rendered := make([][]render.Piece, len(s.Parts))
	lastText := -1 // index of the part holding the last text piece
	for i, p := range s.Parts {
		if p.Type != contract.OutText {
			continue
		}
		pieces, err := render.Render(renderer, p.Text, maxText)
		if err != nil {
			return nil, kit.Permanent("invalid_action")
		}
		rendered[i] = pieces
		if len(pieces) > 0 {
			lastText = i
		}
	}

	var refs []string
	fail := func(err error) ([]string, error) {
		if len(refs) > 0 {
			log.Printf("send action %d failed after sending messages %v: %v", s.ID, refs, err)
		}
		return nil, Classify(err)
	}
	for i, p := range s.Parts {
		if p.Type != contract.OutText {
			mediaRefs, err := a.execMedia(ctx, dest, p, sendOpts{replyTo: replyTo, silent: s.Silent})
			if err != nil {
				return fail(err)
			}
			refs = append(refs, mediaRefs...)
			replyTo = 0
			continue
		}
		for j, piece := range rendered[i] {
			opts := sendOpts{replyTo: replyTo, silent: s.Silent}
			if i == lastText && j == len(rendered[i])-1 {
				opts.buttons = buttons
			}
			id, err := a.sendPiece(ctx, dest, piece, opts)
			if err != nil {
				return fail(err)
			}
			refs = append(refs, strconv.FormatInt(id, 10))
			replyTo = 0 // reply_to only on the first message
		}
	}
	if lastText < 0 && len(buttons) > 0 {
		// No text piece to carry the buttons: they get a message of their own.
		ref, err := a.sendButtonsOnly(ctx, dest, buttons, s.Silent)
		if err != nil {
			return fail(err)
		}
		refs = append(refs, ref)
	}
	return refs, nil
}

// sendPiece sends one piece, resending it as plain text when Telegram rejects its HTML.
func (a *Adapter) sendPiece(ctx context.Context, dest telegram.Destination, piece render.Piece, o sendOpts) (int64, error) {
	p := telegram.SendParams{
		To: dest, Text: piece.Text, ReplyToMessageID: o.replyTo, Buttons: o.buttons, Silent: o.silent,
	}
	if piece.Native {
		p.ParseMode = "HTML"
	}
	id, err := a.bot.SendHTMLEx(ctx, p)
	if err != nil && piece.Native && IsParseEntities(err) {
		p.ParseMode = ""
		p.Text = format.StripTags(piece.Text)
		return a.bot.SendHTMLEx(ctx, p)
	}
	return id, err
}

// toButtons maps contract buttons to Telegram inline buttons (token -> callback_data).
func toButtons(rows [][]contract.Button) ([][]telegram.Button, error) {
	var out [][]telegram.Button
	for _, row := range rows {
		r := make([]telegram.Button, 0, len(row))
		for _, b := range row {
			btn, err := telegram.NewButton(b.Label, b.Token)
			if err != nil {
				return nil, err
			}
			r = append(r, btn)
		}
		out = append(out, r)
	}
	return out, nil
}
