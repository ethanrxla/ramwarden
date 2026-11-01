# Arranging the desktop through a tiling window manager

Users who already run a tiling window manager can ask RamWarden to rearrange
their windows and workspaces, or to edit the window manager's config. RamWarden
never installs or starts a window manager, and the feature is invisible to
anyone who does not run one.

| OS | Window manager | How RamWarden talks to it |
|---|---|---|
| macOS | [AeroSpace](https://github.com/nikitabobko/AeroSpace) | the `aerospace` CLI (`list-windows --json`, `move-node-to-workspace --window-id`, …) |
| Linux | sway, i3 | `swaymsg` / `i3-msg` (`-t get_tree`, `[con_id=N] …`) |
| Windows | [komorebi](https://github.com/LGUG2Z/komorebi) | `komorebic state`, `komorebic …` |

## Contract

- **Detected, never assumed.** The backend is chosen at runtime by asking each
  candidate CLI for its state (`[desktop] backend = "auto"`). A missing CLI or
  a window manager that is not running is `Unsupported`, and every
  `/desktop/*` route answers `{"available": false, "reason": …}` rather than
  failing. `[desktop] enabled = false` turns the feature off entirely.
- **Propose, then apply.** Nothing on the desktop changes in the request that
  describes the change. `POST /desktop/plan` (explicit commands and/or a new
  config) and `POST /desktop/ask` (a sentence for the model) only return a
  *plan*: an id, numbered human-readable steps, and a line diff of any config
  change. `POST /desktop/apply/{id}` carries out that exact plan once. This is
  the "asks before applying a fix from chat" rule: a chat client shows the
  steps and sends `apply` only after the user says yes.
- **Plans are short-lived and single-use.** A plan expires after 10 minutes,
  and applying consumes it. A window named in a plan that has since closed is
  reported as not found; nothing else is substituted for it.
- **What happened, not what was attempted.** After applying, RamWarden reads
  the window manager's state again. A move is `done` only when the window is
  now on the target workspace; a command that the CLI accepted but that changed
  nothing is `unverified`; a failed command is `failed` with the CLI's own
  error text.
- **Config edits are reversible.** Before writing, the current file is copied
  to `<data dir>/ramwarden/wm-backups/`. After writing, the window manager is
  told to reload. If the reload fails, the backup is restored and reloaded, and
  the edit is reported as `refused`. `POST /desktop/config/revert` restores
  the newest backup.

## Policy

- **Vocabulary is closed and non-destructive.** The commands are: focus a
  window, move a window to a workspace, switch workspace, move a workspace to a
  monitor, set a window's layout (tiles, accordion, horizontal, vertical,
  floating, tiling), and balance a workspace. There is no close, quit or kill:
  freeing memory by ending things stays with the ladder and its watchlist,
  whatever a model suggests.
- **No new programs from a config edit.** AeroSpace (`exec-and-forget`,
  `after-startup-command`, `exec-on-workspace-change`), sway/i3 (`exec`,
  `exec_always`) and komorebi configs can launch programs. An edit that *adds*
  a line containing one of those markers is refused, so a model (or a forged
  request) cannot turn a layout change into code execution. Lines that were
  already there are left alone. The user adds such lines by hand.
- **Names are validated before they reach a CLI.** Workspace and monitor names
  are at most 64 characters with no control characters, no leading `-`, and
  none of `; , " [ ]`. sway and i3 take one command *string*, so this is what
  stops a workspace named `x; exec …` from becoming a second command. AeroSpace
  and komorebi receive argv directly and are additionally given `--` where the
  CLI supports it.
- **Model replies are clamped.** `/desktop/ask` sends the model the current
  windows and workspaces and asks for JSON in the same command vocabulary. The
  reply is parsed into commands and validated exactly like a hand-written plan;
  anything outside the vocabulary or naming an unknown window is dropped and
  listed in the plan's notes. At most 50 commands per plan.
- **Freezing is held while applying.** A SIGSTOPped app cannot answer the
  accessibility calls AeroSpace uses to move it, so applying a plan sets the
  same 60-second hold as `POST /hold` and thaws what RamWarden froze first.

## Verification

- Parsers for each backend are unit-tested on recorded output, on every OS.
- The planner is tested against a fake window manager: validation, expiry,
  single use, verification after apply, config backup, refused reload with
  automatic restore, and refusal of added exec lines.
- No test runs a real window manager or touches the user's config.

## Basis and limitations

- An AeroSpace MCP server exists
  ([julianinsua/aerospace-mcp](https://github.com/julianinsua/aerospace-mcp),
  TypeScript, MIT). It also drives the `aerospace` CLI, and its mutating tools
  run immediately once enabled. RamWarden talks to the CLI itself so it needs no
  Node runtime and so every change goes through the propose/apply step above.
  Someone who wants that MCP server in Claude Desktop as well can run it
  alongside; the two do not conflict.
- komorebi's CLI moves only the *focused* window. A komorebi move therefore
  focuses the window first and then moves it, and a window komorebi cannot
  focus is reported as failed.
- GlazeWM (Windows) and Hyprland (Linux) are not implemented yet. Each is one
  more `WindowManager` backend.
- AeroSpace's `reload-config` validates only the file on disk, so a new config
  is checked as TOML before writing and then by the reload itself, which is
  why a failed reload restores the backup.
