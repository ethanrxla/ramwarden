# Tab feedback and review

Automatic closes require fresh extension activity metadata, at least 24 hours idle, a return estimate below 0.3, and all live safety guards. A tab-count target or same-site page cannot bypass this floor. Exact URLs, including query and fragment, define duplicates.

The updated extension reports timestamps and an unsaved-input bit, never typed text. Reopened and explicitly kept pages receive a 24-hour protection, ending on manual close. Local SQLite feedback and outcomes expire after 30 days. Negative feedback can only raise the return estimate; this is not external model retraining.

GET /browser/review offers at most ten uncertain tabs, at most once per 30 minutes. POST /browser/review/respond accepts a token, close/keep/later, and browser/id/url targets. Tokens expire after ten minutes and are single-use. Close rechecks current metadata and uses a correlated, URL-scoped close_safe extension command. Older extensions cannot execute it. Polling-browser automatic closes currently fail closed until correlated acknowledgements are supported.

The Mac pet shows a review bubble; checkboxes start unchecked. Dismissal never authorizes closure. Other clients keep uncertain tabs open.

Validation pending: live disposable-tab browser smoke test, full cross-platform CI, production deployment. Do not replace the installed daemon or extension until those checks pass.
