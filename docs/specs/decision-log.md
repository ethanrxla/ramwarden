# Decision log and reopen recall

## Contract

- Every tab unload, tab close (live or dry-run), app quit (live or dry-run),
  Jev-based app keep, recall-based tab keep, and user "keep" in the close
  review writes one row to the local `decisions` table in `history.db`. Manual
  unloads and closes from the popup, app or API are logged with
  `trigger = "manual"`.
- A row says what happened, not what was attempted: `result` is `confirmed`,
  `queued`, `refused`, `dry_run` or `kept`.
- Each tab row carries the inputs the decision was made from (idle minutes,
  status, closed-form and model return chances, size prior, frecency, site
  class, related/duplicate flags), the reason sentence, Jev's answer as it
  stood (`consulted`, `asked`, `return_24h`, `unsaved`, `veto`, `blended`) or
  why Jev was not consulted, and a 16-dimension decision vector
  (`TabVector`). App rows carry the app features and Jev's four answers.
- Outcomes are written back to the row: an unload the user came back to is
  `returned` (from the existing tab-outcome reconcile), a live close the user
  reopened is `reopened` (from the extension's `POST /browser/feedback`
  `kind: "reopened"`, see `tab-feedback.md`; exact page first, then the newest
  close on the same host, within 7 days). Only the first outcome counts.
- `GET /decisions?kind=&action=&q=&limit=` and `GET /decisions/{id}` serve the
  log locally. The full URL is never in these responses.
- Local rows older than 90 days are deleted.
- `ramwarden-cloud-db decisions sync` copies rows (and ladder `actions` as
  `kind: "ladder"`) to the MongoDB `decisions` collection, keyed
  `<device>:<id>`. Re-syncing is idempotent and late outcomes update the same
  document. Full URLs are never sent. Titles are sent unless `--no-titles`.
- Recall may only hold a close back for one sweep, never cause a close.

## Policy

- Two vector indexes on `decisions`: `decision_features` over `vector` (16
  dims, cosine) and `decision_text` over `text_vector` (an embedding of
  "title · host" by the local Ollama `[ollama] embed_model`, mxbai-embed-large,
  1024 dims, cosine). Filter fields: `kind`, `action`, `result`, `outcome`,
  `device`, `at`.
- Recall, run by `decisions sync` after each sync: for every open tab the
  daemon reports as closeable (`candidate` or `unloaded`), find the 8 nearest
  settled live closes by title embedding (settled = reopened, or older than
  24 h), keep neighbours with score ≥ 0.85, and write
  `{browser, tab_id, url, similar, reopened, examples}` to `recall.json` next
  to `history.db`.
- The sweep reads `recall.json` when it is at most 30 minutes old. A close is
  held back when at least 2 neighbours were reopened and they are at least
  half of the neighbours. The hold is logged as a `keep` decision whose
  `inputs.recall` names the neighbour ids.

## Verification

- `ramwarden-core` `decisions::tests`: round trip, `rev` moves on every
  change, first outcome wins, reopen matching (exact page, then host; dry runs
  and old closes excluded), unload return marking, text filter, vector bounds.
- `ramwarden-daemon` `decisionlog::tests` and `recall::tests`: refused closes
  are logged as refused, a target missing from the report is still logged, dry
  runs are never marked reopened, the hold-back threshold, matching on browser
  + tab id + URL, stale and missing files hold nothing back.
- `ramwarden-cloud` unit tests: the URL never reaches a document, titles are
  optional, the index declares every filter field recall uses, neighbour
  counting. `tests/atlas.rs` (needs `RAMWARDEN_TEST_MONGODB_URI`): sync is
  idempotent and a late outcome updates the same document.
- On a machine with Atlas: `ramwarden-cloud-db init`, then
  `ramwarden-cloud-db decisions sync`, then `decisions recent` lists rows and
  `decisions why "<a tab title>"` returns neighbours once Atlas has built the
  index (a minute or two after `init`).

## Basis and limitations

- The daemon does not link a MongoDB driver and never waits on a network to
  close a tab; the sync runs as its own process. With no sync configured the
  log is complete locally and the sweep is exactly the rule-based one.
- Jev returns probabilities, not prose. "Jev reasoning" in this log is the
  question text it was given and its typed answers.
- Recall needs Ollama with the embed model pulled. Without it documents are
  synced with only the decision vector, `why` and recall are unavailable, and
  `similar` (decision vector) still works.
- Vector search indexes exist only on Atlas or a local Atlas deployment. On a
  plain `mongod` the log syncs but search commands fail.
- The local hour in the decision vector is UTC hour, consistent for one
  machine but not across time zones.
