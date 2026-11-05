# Reviewed workspace workflows

Both companions share a typed workspace catalog, with separate selected folders
and permission scopes. A user can request up to six file/Git steps in one plan:
for example, create a notes folder, write a new file, and show Git status.

The chat response previews numbered steps, exact paths and new file contents.
Git previews also show the fixed system executable and argument array. The
`workflow` operation contains flat `steps`; nested workflows, more than six
steps, and more than 16 KB of new text in total are rejected. Workflow approval
is always manual, including when the companion's single-tool Auto mode is on.
Existing single-operation requests and the JSON action endpoint remain supported.

The same two-minute, single-use token binds the JSON and streaming transports.
Changing or disconnecting the selected folder invalidates outstanding plans.
No operation runs while the model is planning. Each step uses the existing
directory capability, path checks, no-follow file access, and bounded Git runner.
The new `git_log` diagnostic shows ten commit subjects; it accepts no arguments.

`POST /assistant/action/stream` accepts `{ "token": "..." }` and returns SSE
`workspace` events containing JSON. Event `kind` is `plan`, `started`, `result`,
or `finished`. Step indices start at zero. Result events include `succeeded` and
the bounded observed output. The final event reports how many steps succeeded.
The stream is available only through the guarded loopback API.

Steps run sequentially and stop on the first error or nonzero Git exit status.
Completed file changes remain after a failure or cancellation; this is not a
transaction and no automatic rollback is claimed. Disconnecting the stream
stops later steps and drops an active terminal future, whose child is killed.
An unread event channel times out after three seconds instead of holding the
assistant's execution gate indefinitely.
A file operation already dispatched to the blocking pool may finish. If the
connection is lost, the app reports an unconfirmed final outcome and asks the
user to inspect the files before retrying. Approval cannot be replayed to retry.

The Mac chat displays progress, a Stop plan button, and expandable output blocks.
Received results remain in bounded local companion history with an explicit
untrusted-output marker; model context cannot turn them into permission for a
new operation. A lost connection can leave an unreceived result unavailable in
chat. Persisted server-side run recovery is not implemented in this increment.

This adds reviewed multi-step execution, not a general shell. File overwrite,
delete, package installation, elevation, arbitrary argv and autonomous follow-up
plans remain outside this catalog. Windows and Linux clients can consume the
shared API; their native chat surfaces are separate work.
