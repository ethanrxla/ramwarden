# Extension memory breakdown showed “No profiler data yet”

The 0.3.3 popup could show overall RAM usage while reporting an empty memory breakdown. The two panels call different endpoints. On the MacBook, the desktop daemon returned a valid JSON response from `GET /ram-report` with `by_verdict_mb` totals and `available_mb`. The popup only recognized the older `by_category` format and discarded the current report. The screenshot test also returned an empty object for this endpoint and never checked the breakdown.

The popup now shows protected, in-use and idle process memory, plus system-available memory. Zero is a valid measurement. The legacy report renderer remains available for older companions. A failed request displays a load error.

To verify, build the extension with `scripts/build-ext.sh` and run `node extension/tests/popup.cjs` with Playwright Chromium installed. The test opens the packaged popup, serves a synthetic current-format report and checks all four displayed measurements, including zero idle memory.
