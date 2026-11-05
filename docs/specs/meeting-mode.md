# Companion meeting mode

Status: Rust persistence/export foundation implemented; recording is not shipped. Windows capture-exclusion controls
are implemented separately and still need real meeting-app verification.

## Source reviewed

Owner-authorized ScholarCal reference: ethanrxla/smartcalendar at
`d54de198b5c638dd7ba837c0cb70130c96dbc34b`.
Relevant source: `lib/meetings/summary.ts`, `artifactJobs.ts`, `meetingNote.ts`,
`meetingNoteDocument.ts`, and `app/api/meetings/[meetingId]/insights/route.ts`.
Reuse structured summaries, transcript-window insights, idempotent artifact jobs,
and canonical meeting records. Adapt these to Rust services; do not import the
other app's Supabase credentials, user data, or deployment configuration.
ScholarCal's current summary prompt is education-specific; RamWarden summaries
must instead match the actual meeting and distinguish decisions, action items,
open questions and uncertain transcription.

## User flow

- Right-click a companion: Record meeting. Show microphone/system-audio choices,
  permission status and where the meeting will be stored before starting.
- Keep start/stop and a local recording indicator visible to the owner. Meeting
  mode may exclude companion and speech windows from supported screen capture.
  Do not promise invisibility for every sharing method or hide OS privacy signals.
- Stream timestamped transcript segments to a bounded processing queue. Audio
  device loss, denied permissions, disk-full and network failure must be visible.
- Show bounded, deduplicated meeting insights in the upper thought bubble. Clicking
  one opens lower chat with its exact meeting/insight/segment reference and an
  explanation. It does not execute an action or inject transcript instructions.
- Stop finalizes the recording once, then schedules transcription completion and
  summary jobs. Generate a Markdown artifact atomically after successful summary;
  failed jobs remain retryable rather than displaying invented completion.
- Meeting history belongs in an account-isolated retrievable hub, like security
  reports, with local offline access/export. No automatic upload to the developer's
  agent-memory hub. Define retention/deletion and raw-audio handling before capture.

## Processing and storage contract

Use stable meeting IDs, ordered segment IDs/timestamps, recording lifecycle
(created/recording/finalizing/processing/ready/failed) and separate artifact job
states. Claims, retries and completion must be idempotent so a restart cannot
create duplicate summaries or duplicate Markdown files. Keep source references
and distinguish incomplete transcripts from finished ones.

Long transcript processing must use Nyquest with bounded chunks and preserved
speaker/timestamp references. The current adapter only compresses safe assistant
narrative and preserves user/tool content; passing a transcript through it is not
proof the transcript was reduced. Implement a separately reviewed transcript
compression envelope and measure fidelity before claiming transcript compression.
Never let transcript text grant tool permissions or rewrite system policy.

Markdown export: user-selected destination, generated safe filename, no arbitrary
path from a transcript/model, no silent overwrite, UTF-8 and bounded content.
Summaries and retrieval should cite the originating meeting and time ranges.

## Acceptance checklist

- [ ] Rust microphone and loopback capture with explicit start/stop and local status.
- [ ] Durable bounded segment queue; restart, permissions/device loss and disk-full.
- [ ] Transcription, speaker/timestamp handling and Nyquest transcript fidelity.
- [ ] Live insights, deduplication, bubble-to-chat routing and meeting scope isolation.
- [ ] Idempotent finalization/retries and automatic atomic Markdown export.
- [ ] Account-scoped hub list/search/retrieve/delete and offline recovery.
- [ ] Retention controls and no cross-account or developer-hub leakage.
- [ ] Real capture exclusion tests in supported meeting apps; unsupported paths honest.
- [ ] Same flow/artwork/settings on Windows, Mac and Linux where OS support permits.


## Implemented foundation

`crates/ramwarden-daemon/src/meetings.rs` provides an owner-scoped SQLite store,
recording/stopped/processing/ready/failed lifecycle, ordered timestamped transcript
segments with retry deduplication, bounded source-linked insights, lightweight
history listings and deletion. A summary attempt has a five-minute durable lease;
a restarted worker can reclaim an expired lease, and stale/duplicate completions
cannot overwrite a result. Empty transcripts and failed attempts do not become
completed summaries. Completed summaries export atomically to a caller-selected
directory under a generated UUID filename, refusing existing files.

Five regressions cover restart persistence, owner isolation, segment ordering and
replay, stale summary attempts/retries, source-bound insights, deletion, failed
summaries and no-overwrite Markdown export. The daemon library suite has 221 tests
passing and library clippy is clean.

This is an internal storage API, not a recorder or an exposed hosted endpoint.
The `recording` state is lifecycle bookkeeping; it does not turn on an audio
capture device. No database is created until a caller opens a store. No transcript
is uploaded and no microphone is enabled by this increment. Authenticated routes,
OS capture, transcription, Nyquest summarization, automatic export scheduling,
upper-bubble/lower-chat insight interaction and hosted hub sync remain to wire.
Callers must derive the owner scope from authenticated identity, never request
text, and run SQLite work on a blocking worker when adding async HTTP routes.
