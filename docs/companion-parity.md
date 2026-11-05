# Cross-platform companion parity

The RamWarden and ThreatWarden pets must be exactly the same on macOS,
Windows and Linux. Native window chrome may follow the operating system;
the characters must not be independently redesigned or approximated.

The existing Mac companions are the reference until their definition is
extracted into shared, versioned assets. This includes the 48-by-48 pixel
geometry, palette and customization defaults, pupil styles, expressions,
animation frames and timing, integer scale choices (2 through 6, default 3),
transparent margins and status presentation. Compare equivalent state and
settings at the same logical pixel scale; display DPI must not blur pixels.

Confirmed reference behavior:

- RamWarden uses PixelRam in macos/menubar/Companion.swift: 250 ms frames,
  blinking every 25 frames unless thinking, a one-pixel idle bob over eight
  frames, a two-pixel thinking bob, and an alternating speaking mouth.
- RamWarden disconnected opacity is 60%; reconnecting/thinking text appears
  only for those states. There is no permanent dark character nameplate.
- ThreatWarden uses ThreatAvatar in macos/menubar/ThreatCompanion.swift and
  the bundled ThreatWarden.svg, with its own customizable palette. Its
  current avatar has no independent bob animation.
- PetSizing.swift defines integer scale and transparent panel padding.

Release requirements:

- [ ] Extract a single shared sprite/palette/frame definition consumed by all
  three renderers. Do not confuse static website/brand SVG colors with the
  Mac companion defaults.
- [ ] Retain existing Mac customization and settings during migration.
- [ ] Match Windows and Linux to that definition, including all expressions
  and size choices, without platform-only artwork, bobbing or nameplates.
- [ ] Add deterministic golden pixel comparisons for both characters,
  expression frames, palette changes and scales; compare fixed animation
  phases rather than wall-clock screenshot timing.
- [ ] Verify native transparent rendering, dragging, visibility, Pro gating
  and click-to-chat on each platform. Native accessibility text remains
  available even when it is not painted onto the character.

The current uninstalled Windows native companion preview is incomplete and
must not be presented or installed as parity-complete. Linux companion
parity is also pending. Existing Mac pets remain the reference, not evidence
that the other two platform implementations are finished.

## Windows implementation checkpoint

Windows now consumes the shared RAM cell map and the exact ThreatWarden
SVG bundled on Mac (web/assets/pets/threat.svg). The website wardens/threat.svg
is different and is deliberately not used for the companion. RAM idle blink,
bob timing, default palette, and expression frames have regression coverage.
The permanent preview nameplates and invented Threat bob are removed.
Mac now bundles the same RAM base map, retaining its original expression and
customization code. Its app build still needs CI validation for this change.

Native Windows rendering has been visually checked for both pets. Automated
drag injection reports an Explorer hit-test mismatch before delivering input,
so live drag/persistence is not yet verified. Windows palette/size controls,
chat/checklist UI and disconnected presentation remain incomplete; do not
claim full functional parity based on the sprite tests alone.

### Windows interaction/flicker follow-up

Off-screen GDI buffering now publishes a complete frame with one BitBlt,
preventing the transparent-key background from being exposed mid-frame.
The native right-click menu offers Chat, Settings and Hide companion; the
WndProc releases its RefCell borrow before delegating default messages or
pumping the menu, so nested WM_CONTEXTMENU messages are not suppressed.
The menu was observed in the native preview. Pointer/drag events and saved
positions were observed after this follow-up.

Left-click now requests a Chat surface instead of the dashboard. Settings
exposes per-pet integer size, wool/horn/hoof/eye colors, RAM pupil styles and
restore-default controls, saved locally and applied by the host. Chat uses
/assistant/chat with separate per-warden in-window histories and bounded
context. Proposed actions require a separate visible approval; replies do
not execute actions. The running daemon replied to a harmless smoke test,
but reported Cloud AI disabled, so that test used its factual fallback.

Full Mac functional parity is still pending: attached chat-window placement,
persistent chat recall, checklist UI, workspace streaming, voice/mute and
other Mac controls, disconnected presentation and live two-platform checks.

### Dedicated speech boxes and Store audit

Each Windows pet now owns a separate chat/settings window, with its own IPC
channel. Dashboard menu commands retain the dashboard process. Chat prefers
below the pet; proactive thoughts/check-ins/fun facts prefer above, following
live pet bounds and clamping to its monitor work area in physical pixels.
The independent box has transparent margins, rounded outline and a tail.
Thought boxes are opened only for real daemon messages, respect voice mute,
warden identity and pet visibility, avoid taking focus on creation, and are
suppressed while that pet's chat is open. No synthetic production fun facts
are injected to make a demonstration look live.

The dedicated chat window was visually inspected, including the DPI/screen-edge
placement correction. The Store assessment and read-only artifact gate are in
packaging/WINDOWS_STORE.md and scripts/test-windows-store-readiness.ps1. All six
currently installed PE executables were unsigned. This remains a developer
installation using the owner's private provider keys, not a Store-ready release.

Native UI validation: a typed chat message was sent through the dedicated box
and its cloud reply rendered there. The independent thought box rendered a
clearly labeled fixture from an isolated loopback server, not the production
daemon. The installed daemon supports /warden/bubble but /warden/facts returns
404; live newer fun-fact cache/feed behavior therefore remains a backend gate.
