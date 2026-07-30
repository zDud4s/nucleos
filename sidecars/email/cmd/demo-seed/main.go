// Command demo-seed delivers a representative mailbox batch to a running NucleOS daemon.
package main

import (
	_ "embed"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"os"
	"strings"

	"nucleosemail/daemon"
)

const defaultDaemonURL = "http://127.0.0.1:8791"

//go:embed mailbox.json
var mailboxJSON []byte

//go:embed sent.json
var sentJSON []byte

func main() {
	mailbox := flag.String("mailbox", "INBOX", "mailbox name to seed")
	dryRun := flag.Bool("dry-run", false, "print the batch without delivering it")
	flag.Parse()

	batch, err := seededBatch(*mailbox)
	if err != nil {
		log.Fatalf("load embedded mailbox: %v", err)
	}

	if *dryRun {
		printJSON(batch)
		return
	}

	token := strings.TrimSpace(os.Getenv("NUCLEOS_DAEMON_TOKEN"))
	if token == "" {
		log.Fatal("NUCLEOS_DAEMON_TOKEN is required to deliver the demo mailbox")
	}

	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = defaultDaemonURL
	}

	result, err := daemon.New(daemonURL, token).Deliver(batch)
	if err != nil {
		log.Fatalf("deliver demo mailbox: %v", err)
	}
	printJSON(result)
}

func seededBatch(mailbox string) (daemon.Batch, error) {
	var batch daemon.Batch
	if err := json.Unmarshal(mailboxJSON, &batch); err != nil {
		return daemon.Batch{}, err
	}
	batch.Mailbox = mailbox
	return batch, nil
}

func seededSentBatch(mailbox string) (daemon.Batch, error) {
	var batch daemon.Batch
	if err := json.Unmarshal(sentJSON, &batch); err != nil {
		return daemon.Batch{}, err
	}
	batch.Mailbox = mailbox
	return batch, nil
}

func printJSON(value any) {
	encoded, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		log.Fatalf("encode result: %v", err)
	}
	fmt.Println(string(encoded))
}
