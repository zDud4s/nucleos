# TypeSafe Jev request reference

Verified 2026-11. Re-check against the docs (docs.typesafe.ai) if a call starts failing; the
API is versioned and this file is a measurement, not a contract.

## Endpoint and body

`POST https://api.typesafe.ai/v1/systemone`

```json
{
  "model": "jev-latest",
  "state": "<text the model reads>",
  "questions": {
    "<name>": {"type": "noul", "instructions": "...", "criteria": {"true": "...", "false": "..."}}
  }
}
```

- `model` must be explicit. The bare name `jev` is refused. `jev-latest` returned `jev-1.14.0`;
  record the model string the response returns, because `latest` moves.
- Several questions can go in one call. Each is answered independently, so one question's
  wording does not leak into another's answer.

## Question types

| Type | Read from the response | Notes |
|---|---|---|
| `noul` | `answers[name]["noul"]` = P(yes), 0..1 | The default choice. Apply your own thresholds. |
| `choice` | the chosen option plus a confidence | Can collapse to one class (size, 3 classes: "small" chosen 82 of 109). |
| `score` | an ordered-scale value | Came back as a 0-based expected index over the scale. |

Prefer `noul` probabilities with thresholds set from labelled negatives (SKILL.md R4) over
`choice`.

## criteria shapes

- `noul`: optional object `{"true": str, "false": str}`. A plain string is rejected with
  HTTP 422.
- `choice`: required object `{option: description}`.
- `score`: required ordered list of strings, lowest first, at most 10.

Criteria that merely restate the question add nothing and can hurt (SKILL.md R5).

## Price and tokens

$0.042 per 1M input tokens (2026-11). Count tokens from the response. Before a run, estimate
`chars/4` multiplied by 1.4-2, since `chars/4` under-counts (measured about 1.3-1.9x).

## Key handling

Take the key from the environment or the OS credential store. Never print it, log it, put it in
a state, or write it to a file in the repository.

## Reading the response

- Check the HTTP status first; a 422 means the body shape is wrong, not that the model said no.
- A missing or non-numeric `noul` is an error row in an eval and a deny in a judge, never a
  silent zero.
