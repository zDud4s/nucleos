package adapter

import (
	"strconv"

	"nucleoschannelkit/contract"

	"nucleostelegram/telegram"
)

// maxCallbackData is Telegram's own limit on callback_data. Anything longer did not come from a
// button this adapter rendered, so it is dropped rather than forwarded.
const maxCallbackData = 64

// Reasons InboundCallback reports for a dropped callback query; callers count them.
const (
	DropCallbackNoMessage = "callback_no_message"
	DropCallbackNoData    = "callback_no_data"
	DropCallbackTooLong   = "callback_data_too_long"
	DropCallbackNoContext = "callback_no_conversation"
)

// InboundCallback translates a button press into a contract action event. The token is the callback
// data, passed through uninterpreted: it is the core that knows what it means. ok is false when the
// query must be dropped (reason says why): no originating message (too old for Telegram to include
// it), empty or over-long data, or a chat the contract does not model.
func InboundCallback(q telegram.CallbackQuery) (ev contract.Event, ok bool, reason string) {
	if q.Message == nil {
		return contract.Event{}, false, DropCallbackNoMessage
	}
	if q.Data == "" {
		return contract.Event{}, false, DropCallbackNoData
	}
	if len(q.Data) > maxCallbackData {
		return contract.Event{}, false, DropCallbackTooLong
	}
	// The message belongs to the bot; the person who pressed the button is the query's From.
	msg := *q.Message
	presser := q.From
	msg.From = &presser
	conv, sender, ok := conversationAndSender(&msg)
	if !ok {
		return contract.Event{}, false, DropCallbackNoContext
	}

	ref := q.ID
	messageRef := strconv.FormatInt(q.Message.MessageID, 10)
	token := q.Data
	eventID := "cbq-" + q.ID
	typ := "action"
	return contract.Event{
		EventID:      &eventID,
		Conversation: conv,
		Sender:       sender,
		Type:         &typ,
		Action:       &contract.ActionPayload{Ref: &ref, MessageRef: &messageRef, Token: &token},
	}, true, ""
}
