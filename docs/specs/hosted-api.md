# Hosted Jev tier (`ramwarden-api`), phase 1

RamWarden's Jev decisions, answered with the operator's TypeSafe key instead of
one each user brings. Phase 1 of the [paid backend design](https://claude.ai/artifact/JKhG8jLtwhNnZ8h5LdzqvT):
no accounts or payments yet, a fixed token list instead.

## How a daemon uses it

The server speaks Jev's own protocol, so the daemon needs no code change. In
`ramwarden.toml`:

```toml
[jev]
enabled  = true
base_url = "https://api.ramwarden.example"   # the hosted server
```

and the user's RamWarden token goes where the TypeSafe key went: `JEV_API_KEY`
in `secrets.toml`, `.env` or the environment. Never in `ramwarden.toml`.

## What the server enforces

| Rule | Default | Answer when broken |
| --- | --- | --- |
| Bearer token on the list | required | 401 |
| Every question equals a [catalog](../../crates/ramwarden-api/src/catalog.rs) entry (name and definition) | built-in 6 | 422 |
| `state` size | 4096 bytes | 422 |
| Questions per request | 8 | 422 |
| Requests per token per minute | 20 | 429 |
| Requests per token per UTC day | 1000 | 429 |
| Upstream input tokens per UTC day, all users | 20,000,000 (about $0.84 at $0.042/MTok) | 503 |

The model is always the server's `JEV_MODEL`, whatever the request names. Jev's
reply is returned whole, with `x-ramwarden-remaining-today`. A 429 or 503 is a
failed Jev call to the daemon, which already falls back to its closed-form
estimate.

`state` is never logged. Logs carry an 8-hex token label, question names, sizes,
input tokens and latency.

## Running it

```bash
cargo build --release -p ramwarden-api
JEV_API_KEY=...                                   \
RAMWARDEN_API_TOKENS=rw_<random 32+ chars>,...    \
./target/release/ramwarden-api
```

| Variable | Default |
| --- | --- |
| `JEV_API_KEY` | required, from the host's secret store |
| `RAMWARDEN_API_TOKENS` | required, comma-separated, each at least 24 characters |
| `RAMWARDEN_API_BIND` | `0.0.0.0:8080` |
| `JEV_BASE_URL` / `JEV_MODEL` / `JEV_TIMEOUT_SECONDS` | `https://api.typesafe.ai` / `jev-latest` / `10` |
| `RAMWARDEN_API_CATALOG` | unset; a JSON file `{"name": [<question as the daemon sends it>]}` of extra definitions |
| `RAMWARDEN_API_PER_MINUTE` / `_PER_DAY` / `_MAX_STATE_BYTES` / `_MAX_QUESTIONS` / `_DAILY_INPUT_TOKENS` | see the table above |
| `RAMWARDEN_API_LOG` | `ramwarden_api=info` |

Make a token with `openssl rand -hex 24 | sed 's/^/rw_/'`. Serve it behind TLS
(the host's proxy); the server itself speaks plain HTTP.

## When the daemon's questions change

The catalog matches definitions exactly, so new or reworded questions must be
added to `catalog.rs` (or the catalog file) **before** a daemon that sends them
ships, and old ones kept until no released daemon sends them. A question the
server does not know is refused with 422, and the daemon falls back as above.

## Next phases

Phase 2 replaces the token list with accounts and device sign-in on the MongoDB
`Db` in `ramwarden-cloud`, and moves the daily counts to `usage_daily`. Phase 3
adds tiers on top of the same limits.
