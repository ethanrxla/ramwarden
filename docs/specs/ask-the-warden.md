# Ask the warden about the computer

The warden's chat answers questions about what the computer is doing: "what
is my GPU doing?", "what is ollama doing on my computer?", "which model is
loaded?", "did you close Slack?". It answers from live readings taken for
that question, on macOS, Windows and Linux.

Code: `crates/ramwarden-daemon/src/ask/` (probes, redaction, model servers,
plain wording), wired into `POST /assistant/chat` in `assistant.rs`.

## Contract

- **Read-only probes.** Each question is answered from a small set of
  read-only probes. None of them changes anything:

  | Probe | Reads |
  |---|---|
  | `overview` | RAM in use, swap, memory pressure, the 8 largest apps. Runs on every question. |
  | `app(name)` | Every process whose name or app matches, plus its children: memory, CPU % per core over the last sample, VRAM, age, listening ports, connections, focus/audio/window, suspended, RamWarden's classification, idle minutes, and the redacted command lines of the 3 largest. Model servers among them are asked what they serve. |
  | `gpu` | Each NVIDIA GPU: use %, VRAM used/total, temperature, and the processes holding VRAM. On Apple silicon: the GPU's use % and the unified memory it has in use. |
  | `models` | Models loaded now in Ollama and LM Studio, models installed in Ollama, and any llama-server, vLLM, llamafile, koboldcpp or similar server, asked on its own loopback ports (`/v1/models`, and `/slots` for whether llama.cpp is generating). |
  | `history(name)` | RamWarden's last 6 recorded actions on that app. |
  | `hardware` | This computer's CPU model, physical and logical cores (performance and efficiency cores where the CPU has both), total RAM and GPUs. |
  | `devices` | The user's Tailscale devices: name, OS, address, online. |
  | `device(name)` | One other device's CPU, cores and RAM, asked over SSH. See "Other devices". |

- **Inside a process.** For the 3 largest processes of an app, `app` also
  reports who started it (up to 6 parents), its thread count, its open
  handles or file descriptors, and the services it hosts (Windows `svchost`
  services, or the systemd unit on Linux). On Linux it lists up to 8 open
  files by name. The full list with paths stays on the computer.

- **Picking.** The daemon picks probes from the question before asking any
  model: GPU words read the GPU, model words read the model runtimes, CPU and
  core words read the hardware, the name of a Tailscale device reads that
  device instead of this computer, and the name of a running app reads that
  app (at most 2 apps). History words
  ("close", "froze", "did") with an app add its history.
- **One extra round.** The model may answer, or reply
  `{"reply":"","probes":[...]}` to ask for up to 3 more probes, once. A
  request naming anything other than the five probes is ignored. Names are
  limited to 64 characters and treated as data.
- **Size.** Each probe's JSON is capped at 2 KB, dropping the last rows of its
  longest list first and marking it `"truncated": true`.
- **Sources.** Every reply ends with "Looked at: …" naming the probes used,
  and the response carries `looked_at`.
- **Without a model.** RamWarden Lite has no AI. Lite, and any plan where every
  model failed to answer, gets the same readings as plain sentences ("ollama:
  2 processes, 5.1 GB RAM, 4.3 GB VRAM, 2% CPU, serving qwen3:8b, not used for
  14 min."). Lite's reply says conversational answers come with Plus.
  `Entitlement::has_assistant` decides; it follows the pet rule (Plus, Pro, or
  the signed-out self-built app).

## Other devices

"How many CPU cores does my Legion have?" is about another machine.

- **Listing** uses `tailscale status --json` (3 s timeout). No Tailscale, or
  Tailscale not connected, is a normal configuration: the probe says which.
- **Asking a device** is on by default (user decision, 2026-11-16) and off
  with `[assistant] devices_ssh = false`. The daemon runs one fixed,
  read-only command over SSH with the user's own keys:
  - Only a device in Tailscale's own list is ever connected to. A name that
    is not a peer is refused, so neither the question nor a model can point
    SSH at another host.
  - The command is chosen by the device's OS (Linux, macOS, Windows). No text
    from the question or a model reaches the remote shell. Phones are not
    asked.
  - `BatchMode=yes` and `StrictHostKeyChecking=yes`: keys only, never a
    prompt, and a host this computer has not connected to before is refused.
    `known_hosts` is never written.
  - 5 s to connect, 10 s overall, 1 KB of output kept, control characters
    removed.
  - A failure is reported as its cause (unknown host key, keys refused, no SSH
    server, unreachable) with the fix, never as a guess at the hardware.
- **Connected devices pane.** `GET /devices` (loopback only) returns this
  computer's hardware and the device list, and never opens an SSH connection.
  The view model is `ramwarden-ui::devices`. The Windows window has a
  Devices tab; the GTK window and the Mac menu do not show it yet.

## When no model answers

The chat used to say "Cloud AI is opted in but did not respond", which named
no cause. `assistant::ask_model_why` now reports why for each model tried:
routing turned off, the named API key missing, the key rejected (401/403),
the model not found (404), rate limit or credit (429), a timeout with the
model's name, no network, or an unreadable reply; and for the local model:
not set up, not reachable, skipped for VRAM, or timed out. A failed request to
the slower task model (timeout, 400, 404, 422, 5xx, unreadable reply) is
retried once on the conversation model. A rejected key or no network is not
retried. No provider response text or key is ever shown.

## Policy: what a model may see

User decision, 2026-11-14: cloud models may see app and process names,
figures, model names and **redacted** command lines. The same redacted form is
sent to local and cloud models. Never sent: window titles, file paths, file
contents, environment variables.

Redaction (`ask/redact.rs`):
- a path keeps only its last part (`C:\Users\…\models\q.gguf` → `q.gguf`);
- the value of any flag naming a key, token, secret, password, auth, cookie,
  credential, session, bearer, signature or private is `[redacted]`;
- values that look like credentials (`sk-`, `ghp_`, `hf_`, `nvapi-`, JWTs,
  long mixed letter-and-digit tokens) and email addresses are `[redacted]`;
- a URL keeps only `scheme://host:port`;
- at most 24 arguments and 320 characters.

## Verification

- `ask::redact` tests: Windows quoted paths, secret flags in both `--flag v`
  and `--flag=v` forms, bare tokens, emails, URLs with credentials, the cap.
- `ask::probes` tests: picking for GPU, model, app and history questions;
  model requests validated; the size cap.
- `ask::servers` tests: llama-server and vLLM recognised, plain servers not;
  `/v1/models` and `/slots` parsed.
- `ask::plain` tests: each probe worded, including "not running" and runtimes
  that could not be asked.
- `ramwarden_ai::vram` test: `ioreg` statistics parsed.
- `ask::devices` tests: status parsed, a peer found by a word of its name,
  this computer and non-peers never matched, no connection without the
  setting or when offline, SSH failures reduced to causes.
- `ramwarden_kernel::cpuinfo` and `process::detail` tests on a synthetic
  `/proc`; `ramwarden_nt::hardware` and `detail` tests on the test process.
- `ramwarden_ai::routed::failure_tests`: each cause worded, retry rules.
- `ramwarden_ui::devices` tests: the pane's lines.

## Basis and limitations

- The old chat sent one fixed snapshot (RAM and the 10 largest processes). It
  had no CPU, GPU, children, ports or model servers, and the daemon only asked
  Ollama, so on the Windows PC (which serves models with llama-server) "which
  model is running?" had nothing to answer from.
- CPU % is measured over the detector's last sample window, which is 1 to 10
  seconds depending on pressure. Idle minutes count only user-facing use and
  only since the daemon started.
- macOS reports no per-process GPU use without root (`powermetrics`), so the
  GPU probe gives totals there. AMD and Intel GPUs on Windows and Linux are
  not read yet; the probe says so.
- Windows and macOS report how many handles a process has open, not which
  files they are. Open file names are Linux only.
- Another device's hardware is whatever its own commands print. It is shown
  to the model as data and is not parsed.
- Model servers are only asked on loopback, with a 1.5 s timeout. One that does
  not answer is reported as running with its model unknown.
