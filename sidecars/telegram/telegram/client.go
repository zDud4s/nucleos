package telegram

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

type Update struct {
	UpdateID      int64          `json:"update_id"`
	Message       *Message       `json:"message"`
	CallbackQuery *CallbackQuery `json:"callback_query"`
}

type Message struct {
	MessageID int64       `json:"message_id"`
	Chat      Chat        `json:"chat"`
	Text      string      `json:"text"`
	Voice     *Voice      `json:"voice"`
	Document  *Document   `json:"document"`
	Photo     []PhotoSize `json:"photo"`
}

type Voice struct {
	FileID   string `json:"file_id"`
	Duration int    `json:"duration"`
}

type Document struct {
	FileID   string `json:"file_id"`
	FileName string `json:"file_name"`
}

type PhotoSize struct {
	FileID   string `json:"file_id"`
	Width    int    `json:"width"`
	Height   int    `json:"height"`
	FileSize int    `json:"file_size"`
}

type File struct {
	FileID   string `json:"file_id"`
	FilePath string `json:"file_path"`
}

type Chat struct {
	ID int64 `json:"id"`
}

type CallbackQuery struct {
	ID      string   `json:"id"`
	Data    string   `json:"data"`
	Message *Message `json:"message"`
	From    User     `json:"from"`
}

type User struct {
	ID int64 `json:"id"`
}

type Button struct {
	Text         string
	CallbackData string
}

type Client struct {
	token   string
	apiBase string
	http    *http.Client
}

func New(token string) *Client {
	return &Client{
		token:   token,
		apiBase: "https://api.telegram.org",
		http: &http.Client{
			Timeout: 65 * time.Second,
		},
	}
}

// call POSTs a Bot API method with a JSON body and returns the raw result field.
func (c *Client) call(method string, body any) (json.RawMessage, error) {
	encoded, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("encode telegram %s request: %w", method, err)
	}

	request, err := http.NewRequest(http.MethodPost, c.apiBase+"/bot"+c.token+"/"+method, bytes.NewReader(encoded))
	if err != nil {
		return nil, fmt.Errorf("create telegram %s request: %w", method, err)
	}
	request.Header.Set("Content-Type", "application/json")

	response, err := c.http.Do(request)
	if err != nil {
		return nil, fmt.Errorf("perform telegram %s request: %w", method, err)
	}
	defer response.Body.Close()

	if response.StatusCode < http.StatusOK || response.StatusCode >= http.StatusMultipleChoices {
		return nil, fmt.Errorf("telegram %s: status code %d", method, response.StatusCode)
	}

	var envelope struct {
		OK          bool            `json:"ok"`
		Result      json.RawMessage `json:"result"`
		Description string          `json:"description"`
	}
	if err := json.NewDecoder(response.Body).Decode(&envelope); err != nil {
		return nil, fmt.Errorf("decode telegram %s response: %w", method, err)
	}
	if !envelope.OK {
		return nil, fmt.Errorf("telegram %s: %s", method, envelope.Description)
	}

	return envelope.Result, nil
}

func (c *Client) GetUpdates(offset int64, timeoutSecs int) ([]Update, error) {
	result, err := c.call("getUpdates", map[string]any{
		"offset":          offset,
		"timeout":         timeoutSecs,
		"allowed_updates": []string{"message", "callback_query"},
	})
	if err != nil {
		return nil, err
	}

	var updates []Update
	if err := json.Unmarshal(result, &updates); err != nil {
		return nil, fmt.Errorf("decode telegram getUpdates result: %w", err)
	}
	return updates, nil
}

// GetFile resolves a file_id to a downloadable file_path via the Bot API getFile method.
func (c *Client) GetFile(fileID string) (string, error) {
	raw, err := c.call("getFile", map[string]any{"file_id": fileID})
	if err != nil {
		return "", err
	}
	var file File
	if err := json.Unmarshal(raw, &file); err != nil {
		return "", fmt.Errorf("parse getFile response: %w", err)
	}
	return file.FilePath, nil
}

// DownloadFile fetches the raw bytes for a file_path returned by GetFile.
func (c *Client) DownloadFile(filePath string) ([]byte, error) {
	url := c.apiBase + "/file/bot" + c.token + "/" + filePath
	resp, err := c.http.Get(url)
	if err != nil {
		return nil, fmt.Errorf("download file: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("download file: status %d", resp.StatusCode)
	}
	return io.ReadAll(resp.Body)
}

func (c *Client) SendMessage(chatID int64, text string) error {
	_, err := c.call("sendMessage", map[string]any{
		"chat_id": chatID,
		"text":    text,
	})
	return err
}

// SendHTML sends a message with parse_mode=HTML (Telegram renders <b>/<i>/<code>/<pre>/<a>).
func (c *Client) SendHTML(chatID int64, html string) error {
	_, err := c.call("sendMessage", map[string]any{
		"chat_id":    chatID,
		"text":       html,
		"parse_mode": "HTML",
	})
	return err
}

func (c *Client) SendMessageWithButtons(chatID int64, text string, rows [][]Button) error {
	inlineKeyboard := make([][]map[string]string, len(rows))
	for rowIndex, row := range rows {
		inlineKeyboard[rowIndex] = make([]map[string]string, len(row))
		for buttonIndex, button := range row {
			inlineKeyboard[rowIndex][buttonIndex] = map[string]string{
				"text":          button.Text,
				"callback_data": button.CallbackData,
			}
		}
	}

	_, err := c.call("sendMessage", map[string]any{
		"chat_id": chatID,
		"text":    text,
		"reply_markup": map[string]any{
			"inline_keyboard": inlineKeyboard,
		},
	})
	return err
}

func (c *Client) AnswerCallbackQuery(callbackID, text string) error {
	_, err := c.call("answerCallbackQuery", map[string]any{
		"callback_query_id": callbackID,
		"text":              text,
	})
	return err
}
