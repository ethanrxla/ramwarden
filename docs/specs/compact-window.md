# Compact desktop layout and stable interaction

Reference: Python ui/window.py at edf72ec, default 420 × 600, process table above
Suggested cleanup and the goal/action footer. The Rust rewrite preserves that
stacked design; browser recommendations are a short, collapsible section near
the bottom, not a separate notebook page. Advanced process columns remain
available through horizontal scrolling without forcing the window wider.

Browser list viewport: 120px. Rows have a one-line title and short status line;
full URL, reason and browser identity are available in a tooltip. Eligible rows
have checkboxes. Ineligible rows show a status and reason instead of a checkbox
that appears broken. Safety policy remains unchanged.

Periodic refresh must retain row widget identity, keyboard focus, eligible
selection and scroll offsets. Changed safety status clears the selection and
hides its checkbox. Process list refresh must also preserve scroll offsets.
Regression checks exercise real GTK allocations at 420px, select a tab, scroll,
refresh changed data, and confirm selection, row identity and scroll stability.
