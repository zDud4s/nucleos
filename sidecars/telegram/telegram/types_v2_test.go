package telegram

import (
	"encoding/json"
	"testing"
)

func decodeMessage(t *testing.T, raw string) Message {
	t.Helper()
	var m Message
	if err := json.Unmarshal([]byte(raw), &m); err != nil {
		t.Fatalf("decode: %v", err)
	}
	return m
}

func TestV2ChatType(t *testing.T) {
	m := decodeMessage(t, `{"message_id":1,"chat":{"id":-100,"type":"supergroup"}}`)
	if m.Chat.Type != "supergroup" || m.Chat.ID != -100 {
		t.Fatalf("chat = %+v", m.Chat)
	}
}

func TestV2Sticker(t *testing.T) {
	m := decodeMessage(t, `{"message_id":2,"chat":{"id":1},"sticker":{"file_id":"stk","file_unique_id":"u","type":"regular","width":512,"height":512,"is_animated":false,"is_video":false,"emoji":"x","file_size":12345}}`)
	if m.Sticker == nil || m.Sticker.FileID != "stk" || m.Sticker.FileSize != 12345 {
		t.Fatalf("sticker = %+v", m.Sticker)
	}
}

func TestV2VideoNote(t *testing.T) {
	m := decodeMessage(t, `{"message_id":3,"chat":{"id":1},"video_note":{"file_id":"vn","length":240,"duration":7,"file_size":99000}}`)
	if m.VideoNote == nil || m.VideoNote.FileID != "vn" || m.VideoNote.Duration != 7 || m.VideoNote.FileSize != 99000 {
		t.Fatalf("video_note = %+v", m.VideoNote)
	}
}

func TestV2Audio(t *testing.T) {
	m := decodeMessage(t, `{"message_id":4,"chat":{"id":1},"audio":{"file_id":"au","duration":180,"performer":"p","title":"t","file_name":"song.mp3","mime_type":"audio/mpeg","file_size":4000000}}`)
	a := m.Audio
	if a == nil || a.FileID != "au" || a.FileName != "song.mp3" || a.MimeType != "audio/mpeg" || a.FileSize != 4000000 || a.Duration != 180 {
		t.Fatalf("audio = %+v", a)
	}
}

func TestV2Voice(t *testing.T) {
	m := decodeMessage(t, `{"message_id":5,"chat":{"id":1},"voice":{"file_id":"v","duration":3,"mime_type":"audio/ogg","file_size":8000}}`)
	if m.Voice == nil || m.Voice.MimeType != "audio/ogg" || m.Voice.FileSize != 8000 || m.Voice.Duration != 3 {
		t.Fatalf("voice = %+v", m.Voice)
	}
}

func TestV2DocumentAndFile(t *testing.T) {
	m := decodeMessage(t, `{"message_id":6,"chat":{"id":1},"document":{"file_id":"d","file_name":"a.pdf","mime_type":"application/pdf","file_size":77}}`)
	if m.Document == nil || m.Document.MimeType != "application/pdf" || m.Document.FileSize != 77 {
		t.Fatalf("document = %+v", m.Document)
	}
	var f File
	if err := json.Unmarshal([]byte(`{"file_id":"f","file_path":"p/x","file_size":5}`), &f); err != nil || f.FileSize != 5 {
		t.Fatalf("file = %+v err=%v", f, err)
	}
}

func TestV2VideoAnimationContactLocationPoll(t *testing.T) {
	m := decodeMessage(t, `{"message_id":7,"chat":{"id":1},
"video":{"file_id":"vid","duration":4,"file_name":"v.mp4","mime_type":"video/mp4","file_size":10},
"animation":{"file_id":"gif","file_name":"g.mp4","mime_type":"video/mp4","file_size":11},
"contact":{"phone_number":"+351","first_name":"Ana","last_name":"B","user_id":9},
"location":{"latitude":38.7,"longitude":-9.1},
"poll":{"id":"p1","question":"q?","options":[{"text":"a","voter_count":1},{"text":"b","voter_count":0}]}}`)
	if m.Video == nil || m.Video.FileID != "vid" || m.Video.MimeType != "video/mp4" {
		t.Fatalf("video = %+v", m.Video)
	}
	if m.Animation == nil || m.Animation.FileID != "gif" {
		t.Fatalf("animation = %+v", m.Animation)
	}
	if m.Contact == nil || m.Contact.PhoneNumber != "+351" || m.Contact.FirstName != "Ana" || m.Contact.UserID != 9 {
		t.Fatalf("contact = %+v", m.Contact)
	}
	if m.Location == nil || m.Location.Latitude != 38.7 || m.Location.Longitude != -9.1 {
		t.Fatalf("location = %+v", m.Location)
	}
	if m.Poll == nil || m.Poll.Question != "q?" || len(m.Poll.Options) != 2 || m.Poll.Options[0].Text != "a" {
		t.Fatalf("poll = %+v", m.Poll)
	}
}

func TestV1UnchangedSurface(t *testing.T) {
	m := decodeMessage(t, `{"message_id":10,"message_thread_id":4,"is_topic_message":true,"chat":{"id":55},"from":{"id":8,"is_bot":false},"text":"hi","caption":"cap","voice":{"file_id":"v","duration":2},"document":{"file_id":"d","file_name":"n.txt"},"photo":[{"file_id":"p","width":1,"height":2,"file_size":3}],"reply_to_message":{"message_id":9,"chat":{"id":55}}}`)
	if m.MessageID != 10 || m.Chat.ID != 55 || m.Text != "hi" || m.Caption != "cap" || m.SenderID() != 8 {
		t.Fatalf("message = %+v", m)
	}
	if m.Voice.FileID != "v" || m.Voice.Duration != 2 || m.Document.FileName != "n.txt" {
		t.Fatalf("attachments = %+v %+v", m.Voice, m.Document)
	}
	if len(m.Photo) != 1 || m.Photo[0].FileSize != 3 || m.ReplyToMessage == nil || m.ReplyToMessage.MessageID != 9 {
		t.Fatalf("photo/reply = %+v", m)
	}
	if d := m.Destination(); d.ChatID != 55 || d.ThreadID != 4 {
		t.Fatalf("destination = %+v", d)
	}
	if m.Sticker != nil || m.Audio != nil || m.Poll != nil {
		t.Fatal("v2 fields must stay nil when absent")
	}
}
