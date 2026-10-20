package kit

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"strings"

	"nucleoschannelkit/contract"
)

var (
	// ErrMediaTooLarge: the media is over the limit; the adapter sends the part as unsupported.
	ErrMediaTooLarge = errors.New("channelkit: media too large")
	// ErrMediaNotFound: the core has no such media for this channel.
	ErrMediaNotFound = errors.New("channelkit: media not found")
)

// bareMIME drops the parameters of a media type ("text/plain; charset=utf-8" -> "text/plain").
func bareMIME(m string) string {
	if base, _, err := mime.ParseMediaType(m); err == nil {
		return base
	}
	base, _, _ := strings.Cut(m, ";")
	return strings.TrimSpace(base)
}

// UploadMedia sends raw bytes to POST /channels/{channel}/media with the mime
// as Content-Type (without parameters) and returns the core's media id.
func (c *Client) UploadMedia(ctx context.Context, mimeType string, r io.Reader) (string, error) {
	body, status, err := c.do(ctx, http.MethodPost, c.channelPath("/media"), r, bareMIME(mimeType))
	if err != nil {
		return "", fmt.Errorf("%w: upload media: %w", ErrTransient, err)
	}
	switch Classify(EndpointMedia, status, body) {
	case OK:
	case SendAsUnsupported:
		return "", ErrMediaTooLarge
	case Retry:
		return "", fmt.Errorf("%w: upload media: status %d", ErrTransient, status)
	default:
		return "", fmt.Errorf("%w: upload media: status %d", ErrFatal, status)
	}
	resp, err := contract.DecodeLenient[contract.MediaResponse](body)
	if err != nil {
		return "", fmt.Errorf("%w: media response: %w", ErrFatal, err)
	}
	return *resp.MediaID, nil
}

// DownloadMedia fetches GET /channels/{channel}/media/{id}. Content over max
// bytes yields ErrMediaTooLarge: the media is never truncated. At most max+1
// bytes are ever read into memory.
func (c *Client) DownloadMedia(ctx context.Context, id string, max int64) (io.ReadCloser, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.baseURL+c.channelPath("/media/"+url.PathEscape(id)), nil)
	if err != nil {
		return nil, c.redact(fmt.Errorf("create request: %w", err))
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%w: download media: %w", ErrTransient, c.redact(err))
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		errBody, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		switch Classify(EndpointMedia, resp.StatusCode, errBody) {
		case NotFound:
			return nil, ErrMediaNotFound
		case SendAsUnsupported:
			return nil, ErrMediaTooLarge
		case Retry:
			return nil, fmt.Errorf("%w: download media: status %d", ErrTransient, resp.StatusCode)
		default:
			return nil, fmt.Errorf("%w: download media: status %d", ErrFatal, resp.StatusCode)
		}
	}
	if resp.ContentLength > max {
		return nil, ErrMediaTooLarge
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, max+1))
	if err != nil {
		return nil, fmt.Errorf("%w: download media: read: %w", ErrTransient, c.redact(err))
	}
	if int64(len(data)) > max {
		return nil, ErrMediaTooLarge
	}
	return io.NopCloser(bytes.NewReader(data)), nil
}
