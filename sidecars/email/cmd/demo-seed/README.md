# Demo mailbox seed

This development-only command sends an embedded mailbox corpus to a running NucleOS daemon. It uses no IMAP settings and is not used by the production email sidecar.

Inspect the corpus offline, with no daemon or token required:

```sh
cd sidecars/email
go run ./cmd/demo-seed -dry-run
```

Deliver it to the default local daemon:

```sh
cd sidecars/email
NUCLEOS_DAEMON_TOKEN=<local-api-bearer-token> go run ./cmd/demo-seed
```

Set `NUCLEOS_DAEMON_URL` to use a daemon other than `http://127.0.0.1:8791`, and use `-mailbox` to name the seeded mailbox. The only token scope needed is permission to call the local daemon's `POST /email/incoming` endpoint.

The daemon stores exactly the UIDs it receives, so re-seeding after the cursor has moved requires a corpus with UIDs above that cursor.
