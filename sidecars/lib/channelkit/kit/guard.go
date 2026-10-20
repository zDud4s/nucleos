package kit

import "nucleoschannelkit/contract"

// ReasonUnsupportedCapability is the failure reason of an action that uses a
// capability the adapter did not declare in its hello (grammar T7).
const ReasonUnsupportedCapability = "unsupported_capability"

// Guard is the defensive capability check (D-B7). The core degrades an action to
// what the hello declared (S 6.5), so a violation means a core bug or a stale
// hello: the action is acked as a permanent failure instead of reaching Execute.
// It returns nil when the action is fine. A text longer than max_text is not a
// violation: chunking belongs to the adapter. It matches Runner.Guard.
func Guard(caps contract.Capabilities, a contract.Action) *contract.Ack {
	if violates(caps, a) {
		return &contract.Ack{Failed: &contract.Failed{Retryable: false, Reason: ReasonUnsupportedCapability}}
	}
	return nil
}

func violates(caps contract.Capabilities, a contract.Action) bool {
	switch v := a.(type) {
	case contract.Send:
		return (len(v.Buttons) > 0 && !caps.Buttons) ||
			(v.ReplyToPlatformRef != "" && !caps.ReplyTo) ||
			mediaUnsupported(caps, v.Parts)
	case contract.Edit:
		return !caps.Edit || (len(v.Buttons) > 0 && !caps.Buttons) || mediaUnsupported(caps, v.Parts)
	}
	return false
}

// mediaUnsupported reports whether a part is an image, file or audio the adapter
// cannot send.
func mediaUnsupported(caps contract.Capabilities, parts []contract.OutPart) bool {
	for _, p := range parts {
		var kind contract.MediaKind
		switch p.Type {
		case contract.OutImage:
			kind = contract.MediaImage
		case contract.OutFile:
			kind = contract.MediaFile
		case contract.OutAudio:
			kind = contract.MediaAudio
		default:
			continue
		}
		found := false
		for _, k := range caps.MediaOut {
			if k == kind {
				found = true
				break
			}
		}
		if !found {
			return true
		}
	}
	return false
}
