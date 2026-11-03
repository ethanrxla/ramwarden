# Opt-in usage diagnostics

A signed-in user may choose wallet, email or verified linked methods. Payment
choice is independent. A registered device token, rather than an email address,
wallet signature, API key or MongoDB credential, authenticates diagnostics.

`share_usage` is off by default. The account preference endpoint owns the
`usage_since` timestamp and changes it on a new opt-in. It cannot be backdated
by a client. Disabling consent rejects subsequent uploads immediately. Connecting
a device and opting into diagnostics are separate actions.

The client aggregates local SQLite records into daily numeric summaries. It
checks every five minutes and retries the last seven days, since the current
consent timestamp. No title, URL, hostname, app name, file path, arbitrary reason,
page content or free-text field is in the upload schema. The server rejects
unknown fields and counters. Daily summaries are self-reported diagnostics;
they never authorize a payment, unlock a plan, or cause a computer action.

The counters distinguish confirmed operations, refusals, dry runs, keep/reopen
feedback, and bytes measurably recovered. `tab_close_confirmations` counts
confirmation notifications: older browser workers can report both an extension
confirmation and a daemon acknowledgement, so it is not a unique-tab count.
`close_decisions_confirmed` and `close_decisions_reopened` describe the local
structured decision log and its subsequent outcome. Never add these different
stages together and label the result as unique tabs closed.

## Hosted API and storage

- Device bearer: `GET /v1/usage/config`, `POST /v1/usage/daily`.
- Account session: `GET /v1/usage` and the existing preferences endpoint.
- Uploads contain at most seven days, at most 30 days old, and must use the
  server's current consent timestamp. Unknown text fields are rejected.
- The server supplies account, device and platform; clients cannot choose owners.
- Four uploads/minute and 1,000/day/device are independent of the AI budget.
- `device_usage_daily` has one document per device/day/consent period. Atomic
  maxima prevent retries and out-of-order snapshots inflating counts. A new
  consent period has its own document, preventing an off/on cycle from mixing
  counter baselines.
- TTL expiry is 90 days after the day's end. Account/day lookup is indexed.
  Chat/ticket retention and payment records are separate.

## Collector

`ramwarden-usage-sync login` displays the normal device approval page and saves
its token privately. Already signed-in website users explicitly approve the named
computer; signed-out users use wallet or email. This never enables diagnostics.

`ramwarden-usage-sync --watch` runs at background priority on Mac through
`com.ramwarden.usage`. It requires a registered device and account consent before
reading local activity. It sends no database/provider credentials. The same
binary supports Windows and Linux; their service packaging can use `--watch`.
`RAMWARDEN_ACCOUNT_API` overrides the official HTTPS origin for self-hosting or
HTTP loopback development. TLS validation remains enabled.
