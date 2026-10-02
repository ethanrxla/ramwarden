# Manual tab selection and closing

Manual closing is separate from automatic unloading. Recent tabs and legacy
extension tabs may be manually selected without satisfying the idle/discard
metadata requirements. Known active, pinned, audible, private and protected-URL
tabs remain protected. Stale reports cannot be acted on: visible polling requests
fresh socket reports automatically, and closing refreshes/revalidates targets.

A compact Close button shows a confirmation with the tab titles and unsaved-work
warning. Cancelling sends nothing. Close dispatch uses only browser+ID+URL
matches from fresh reports. Legacy bare-ID commands are accepted only for IDs
with a unique owner; ambiguous IDs or navigation changes are refused. Only
browser-confirmed IDs count as closed; polling delivery remains queued. No
actual user tabs are closed during verification.

Tests cover legacy/recent selection, independent unload eligibility, guards,
navigation and ownership checks, browser-confirmed close responses, retained
checkboxes after refresh, and 420px layout.
