package telegram

import (
	"context"
	"encoding/json"
	"fmt"
)

// maxCallbackDataBytes is Telegram's limit on an inline button's callback_data. It counts bytes,
// not characters, and Telegram answers an oversized one with a 400 for the whole message.
const maxCallbackDataBytes = 64

// SendParams describes one outgoing message for SendHTMLEx.
type SendParams struct {
	To               Destination
	Text             string
	ParseMode        string
	ReplyToMessageID int64
	Buttons          [][]Button
	Silent           bool
}

// NewButton builds an inline button and refuses callback data Telegram would reject.
func NewButton(text, callbackData string) (Button, error) {
	if len(callbackData) > maxCallbackDataBytes {
		return Button{}, fmt.Errorf("callback_data is %d bytes, Telegram allows at most %d", len(callbackData), maxCallbackDataBytes)
	}
	return Button{Text: text, CallbackData: callbackData}, nil
}

// inlineMarkup validates the buttons and renders reply_markup; nil when there are no buttons.
// Buttons built as literals bypass NewButton, so the limit is enforced here as well, before any
// request is made.
func inlineMarkup(rows [][]Button) (map[string]any, error) {
	if len(rows) == 0 {
		return nil, nil
	}
	keyboard := make([][]map[string]string, len(rows))
	for r, row := range rows {
		keyboard[r] = make([]map[string]string, len(row))
		for b, button := range row {
			if _, err := NewButton(button.Text, button.CallbackData); err != nil {
				return nil, fmt.Errorf("button %q: %w", button.Text, err)
			}
			keyboard[r][b] = map[string]string{"text": button.Text, "callback_data": button.CallbackData}
		}
	}
	return map[string]any{"inline_keyboard": keyboard}, nil
}

// SendHTMLEx sends a message and returns the id Telegram gave it.
func (c *Client) SendHTMLEx(ctx context.Context, p SendParams) (int64, error) {
	request := p.To.body()
	request["text"] = p.Text
	if p.ParseMode != "" {
		request["parse_mode"] = p.ParseMode
	}
	if p.Silent {
		request["disable_notification"] = true
	}
	if p.ReplyToMessageID != 0 {
		// A reply to a message that has since been deleted must still be delivered, not fail with
		// "message to be replied not found".
		request["reply_parameters"] = map[string]any{
			"message_id":                  p.ReplyToMessageID,
			"allow_sending_without_reply": true,
		}
	}
	markup, err := inlineMarkup(p.Buttons)
	if err != nil {
		return 0, err
	}
	if markup != nil {
		request["reply_markup"] = markup
	}

	raw, err := c.callContext(ctx, "sendMessage", request)
	if err != nil {
		return 0, err
	}
	var sent struct {
		MessageID int64 `json:"message_id"`
	}
	if err := json.Unmarshal(raw, &sent); err != nil {
		return 0, fmt.Errorf("decode telegram sendMessage result: %w", err)
	}
	return sent.MessageID, nil
}

// EditMessageText replaces the text (and keyboard) of a message the bot sent earlier.
func (c *Client) EditMessageText(ctx context.Context, chatID, messageID int64, text, parseMode string, buttons [][]Button) error {
	request := map[string]any{"chat_id": chatID, "message_id": messageID, "text": text}
	if parseMode != "" {
		request["parse_mode"] = parseMode
	}
	markup, err := inlineMarkup(buttons)
	if err != nil {
		return err
	}
	if markup != nil {
		request["reply_markup"] = markup
	}
	_, err = c.callContext(ctx, "editMessageText", request)
	return err
}

// SendChatAction shows an activity indicator such as "typing".
func (c *Client) SendChatAction(ctx context.Context, to Destination, action string) error {
	request := to.body()
	request["action"] = action
	_, err := c.callContext(ctx, "sendChatAction", request)
	return err
}

// AnswerCallbackQueryCtx is AnswerCallbackQuery with a context.
func (c *Client) AnswerCallbackQueryCtx(ctx context.Context, callbackID, text string) error {
	_, err := c.callContext(ctx, "answerCallbackQuery", map[string]any{
		"callback_query_id": callbackID,
		"text":              text,
	})
	return err
}
