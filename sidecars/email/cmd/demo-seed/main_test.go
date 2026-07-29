package main

import (
	"encoding/json"
	"strings"
	"testing"

	"nucleosemail/daemon"
)

func TestEmbeddedMailboxCoversTriagePaths(t *testing.T) {
	var batch daemon.Batch
	if err := json.Unmarshal(mailboxJSON, &batch); err != nil {
		t.Fatalf("embedded mailbox must parse as daemon.Batch: %v", err)
	}

	noiseSignals := map[string]bool{}
	classes := map[string]bool{}
	maxUID := uint32(0)

	for _, message := range batch.Messages {
		if message.UID > maxUID {
			maxUID = message.UID
		}
		for _, class := range []string{"urgent", "action", "info", "noise"} {
			if strings.Contains(message.Subject, "["+class+"]") {
				classes[class] = true
			}
		}
		if _, ok := message.Headers["List-Unsubscribe"]; ok {
			noiseSignals["List-Unsubscribe"] = true
		}
		if message.Headers["Precedence"] == "bulk" {
			noiseSignals["Precedence: bulk"] = true
		}
		if message.Headers["Auto-Submitted"] == "auto-replied" {
			noiseSignals["Auto-Submitted: auto-replied"] = true
		}
		if strings.HasPrefix(message.FromAddr, "no-reply@") {
			noiseSignals["no-reply sender"] = true
		}
	}

	for _, skipped := range batch.Skipped {
		if skipped.UID > maxUID {
			maxUID = skipped.UID
		}
	}

	for _, signal := range []string{
		"List-Unsubscribe",
		"Precedence: bulk",
		"Auto-Submitted: auto-replied",
		"no-reply sender",
	} {
		if !noiseSignals[signal] {
			t.Errorf("missing deterministic noise signal %q", signal)
		}
	}
	for _, class := range []string{"urgent", "action", "info", "noise"} {
		if !classes[class] {
			t.Errorf("missing %q class intention", class)
		}
	}
	if batch.MaxUIDExamined < maxUID {
		t.Errorf("max_uid_examined %d is behind highest seeded uid %d", batch.MaxUIDExamined, maxUID)
	}
}

func TestDryRunBatchMarshals(t *testing.T) {
	batch, err := seededBatch("demo-mailbox")
	if err != nil {
		t.Fatalf("load embedded mailbox: %v", err)
	}
	if _, err := json.MarshalIndent(batch, "", "  "); err != nil {
		t.Fatalf("dry-run batch must marshal: %v", err)
	}
}
