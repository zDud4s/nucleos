// Package adapter is the v2 channel adapter for Telegram, built on the shared channelkit.
package adapter

import "nucleoschannelkit/contract"

// maxText is Telegram's limit on the text of a single message.
const maxText = 4096

// Capabilities is what the Telegram adapter declares in its hello. It must equal
// the shared fixture hello/telegram-full.json by value.
func Capabilities() contract.Capabilities {
	m := maxText
	return contract.Capabilities{
		Buttons:  true,
		Edit:     true,
		Typing:   true,
		Threads:  true,
		ReplyTo:  true,
		MaxText:  &m,
		MediaIn:  []contract.MediaKind{contract.MediaAudio, contract.MediaImage, contract.MediaFile},
		MediaOut: []contract.MediaKind{contract.MediaImage, contract.MediaFile, contract.MediaAudio},
	}
}

// Hello is the adapter's presentation to the core.
func Hello() contract.Hello {
	c := Capabilities()
	return contract.Hello{Kind: "telegram", Version: contract.Version, Capabilities: &c}
}
