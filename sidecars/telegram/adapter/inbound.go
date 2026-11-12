package adapter

import (
	"strconv"
	"strings"
	"time"

	"nucleoschannelkit/contract"

	"nucleostelegram/telegram"
)

// Inbound translates a Telegram update into a contract event.
//
// needsMedia is true when the message carries media (voice, photo, document, audio) that the caller
// must attach with AttachMedia before posting; the returned event then holds only the text parts.
// ok is false when the update must be dropped: no message, no usable sender (fail shut, like v1), or
// neither text nor media nor an unsupported part to report.
func Inbound(update telegram.Update) (ev contract.Event, needsMedia bool, ok bool) {
	msg := update.Message
	if msg == nil {
		return contract.Event{}, false, false
	}
	conv, sender, ok := conversationAndSender(msg)
	if !ok {
		return contract.Event{}, false, false
	}

	var parts []contract.InPart
	if text := messageText(msg); text != "" {
		parts = append(parts, textPart(text))
	}
	if label := unsupportedLabel(msg); label != "" {
		parts = append(parts, unsupportedPart(label))
	}
	needsMedia = hasMedia(msg)
	if len(parts) == 0 && !needsMedia {
		return contract.Event{}, false, false
	}

	ref := strconv.FormatInt(msg.MessageID, 10)
	message := &contract.Message{Ref: &ref, Parts: parts}
	if msg.ReplyToMessage != nil {
		replyTo := strconv.FormatInt(msg.ReplyToMessage.MessageID, 10)
		message.ReplyTo = &replyTo
	}
	if message.Parts == nil {
		message.Parts = []contract.InPart{}
	}
	eventID := "upd-" + strconv.FormatInt(update.UpdateID, 10)
	typ := "message"
	return contract.Event{
		EventID:      &eventID,
		At:           messageTime(msg),
		Conversation: conv,
		Sender:       sender,
		Type:         &typ,
		Message:      message,
	}, needsMedia, true
}

// conversationAndSender derives the conversation and the sender of a message. It is shared with the
// callback translation so both name conversations identically.
//
// The topic is taken only through Message.Destination, which reads MessageThreadID solely when
// IsTopicMessage is set: a reply chain in an ordinary supergroup carries a thread id too, and treating
// it as a topic would give each chain a conversation of its own. ok is false when there is no sender
// (channel posts, anonymous admins) or the chat type is not one the contract models.
func conversationAndSender(msg *telegram.Message) (*contract.Conversation, *contract.Sender, bool) {
	if msg == nil || msg.From == nil {
		return nil, nil, false
	}
	var kind contract.ConversationKind
	switch msg.Chat.Type {
	case "private":
		kind = contract.ConvPrivate
	case "group", "supergroup":
		kind = contract.ConvGroup
	default:
		return nil, nil, false
	}
	dest := msg.Destination()
	chat := strconv.FormatInt(dest.ChatID, 10)
	conv := &contract.Conversation{Ref: &chat, Kind: &kind}
	if dest.ThreadID != 0 {
		ref := chat + ":" + strconv.FormatInt(dest.ThreadID, 10)
		parent := chat
		thread := contract.ConvThread
		conv = &contract.Conversation{Ref: &ref, Kind: &thread, Parent: &parent}
	}
	sref := strconv.FormatInt(msg.From.ID, 10)
	return conv, &contract.Sender{Ref: &sref, Name: senderName(msg.From)}, true
}

// senderName is the display name the contract's sender.name carries: first and last name joined by a
// space, else whichever of them is set, else the @username, else nil (the field is optional).
func senderName(u *telegram.User) *string {
	name := strings.TrimSpace(strings.TrimSpace(u.FirstName) + " " + strings.TrimSpace(u.LastName))
	if name == "" {
		name = strings.TrimSpace(u.Username)
	}
	if name == "" {
		return nil
	}
	return &name
}

// messageTime is the message's date as the contract spells "at" (RFC3339, UTC), or nil when Telegram
// sent none.
func messageTime(msg *telegram.Message) *string {
	if msg.Date <= 0 {
		return nil
	}
	at := time.Unix(msg.Date, 0).UTC().Format(time.RFC3339)
	return &at
}

// messageText is the text of a message, or its caption when the caption has no part of its own to ride
// on. The contract gives image and file a caption field, so on a photo or a document AttachMedia puts
// the caption there and it is not repeated as text; a voice note, an audio file or a message the
// contract only reports as unsupported has no such field, and its caption is its text.
func messageText(msg *telegram.Message) string {
	if msg.Text != "" {
		return msg.Text
	}
	if len(msg.Photo) > 0 || msg.Document != nil {
		return ""
	}
	return msg.Caption
}

func hasMedia(msg *telegram.Message) bool {
	return msg.Voice != nil || msg.Document != nil || len(msg.Photo) > 0 || msg.Audio != nil
}

// unsupportedLabel names the first thing in the message the contract has no part for, or "".
func unsupportedLabel(msg *telegram.Message) string {
	switch {
	case msg.Sticker != nil:
		return "sticker"
	case msg.Video != nil:
		return "video"
	case msg.VideoNote != nil:
		return "video_note"
	case msg.Animation != nil:
		return "animation"
	case msg.Contact != nil:
		return "contact"
	case msg.Location != nil:
		return "location"
	case msg.Poll != nil:
		return "poll"
	}
	return ""
}

func textPart(text string) contract.InPart {
	typ := contract.PartText
	return contract.InPart{Type: &typ, Text: &text}
}

func unsupportedPart(label string) contract.InPart {
	typ := contract.PartUnsupported
	return contract.InPart{Type: &typ, Label: &label}
}
