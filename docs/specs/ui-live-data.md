# Live desktop data and analysis

## Failure
The Rust daemon answers `/health` and `/state`, but the GTK window remains
blank and Analyse never completes. Startup drives a current-thread Tokio
runtime with `block_on(verify)`, then only enters it while GTK owns the thread.
Entering a runtime does not drive its I/O or timers.

## Required behavior
- A runtime with independently running I/O and timer drivers lives for the
  entire GTK application run. GTK widgets remain on the GTK thread.
- The first successful state response renders memory usage, verdict totals,
  pressure, compression savings, browser count, and process rows.
- Subsequent polling updates the display. Failed requests show an error and
  later successful requests recover automatically.
- Analyse completes with a summary or a bounded transport error; duplicate
  clicks/Enter do not start concurrent analyses. The control becomes available
  again after success or failure.
- Startup has visible loading text. Connection failures and analysis outcomes
  are logged as well as displayed. Logs must not include the user's goal.
- An optional watchlist request cannot delay the first metrics request.

## Regression verification
Use a local HTTP fixture and the real GTK/GLib loop, with the same runtime
constructor as production. Delay fixture responses to require I/O wakeups.
Assert populated labels/table, repeated polling, analysis success and error,
re-enabled analysis, visible state failure and recovery. A GLib watchdog must
fail the test if Tokio stops making progress. Run under Xvfb; no real model
calls or process mutations. Also verify a Tokio timer completes while GLib
owns the main thread, and run the existing workspace tests.
