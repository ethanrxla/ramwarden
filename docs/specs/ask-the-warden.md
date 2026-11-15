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

- **Picking.** The daemon picks probes from the question before asking any
  model: GPU words read the GPU, model words read the model runtimes, and the
  name of a running app reads that app (at most 2 apps). History words
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
- Model servers are only asked on loopback, with a 1.5 s timeout. One that does
  not answer is reported as running with its model unknown.
