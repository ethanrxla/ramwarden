# Terminal setup and optional remote devices

`ramwarden setup` opens the Rust guided walkthrough. Run it in an interactive terminal. It checks the local daemon, connects the account through browser approval, offers the native Mac setup guide when available, and explains how to add other computers. It does not buy a plan or enable diagnostics sharing.

- `ramwarden doctor` / `ramwarden-doctor`: read-only daemon, account and optional Tailscale checks.
- `ramwarden status`: current measured RAM and reported ThreatWarden status.
- `ramwarden watch`: a lightweight live terminal screen; Ctrl-C returns to the previous terminal screen.
- `ramwarden login`: connect this machine with its own device approval.
- `ramwarden tailscale`: select visible peers and prepare a copyable request for the user's agent.

The guide uses crimson and purple accents when the output is a terminal. Redirected output is plain text; `NO_COLOR` disables color. Nothing runs continuously unless the user explicitly chooses `watch`.

## Customer-owned Tailscale

Ordinary installation uses outbound HTTPS and does not require Tailscale. Users who already have Tailscale keep their own tailnet; never join customers to the operator's personal network. GitHub sign-in authenticates a Warden account and does not grant SSH access.

The picker reads `tailscale status --json`, omits this computer, labels offline peers, and checks the remaining allowance against the authenticated account. Plus allows 3 computers total; Pro allows 5, including machines already connected. The generated agent request scopes work to the selected machines, requires existing authorized SSH access and each machine's browser/admin approval, and forbids copying tokens between devices. It executes no remote commands and changes no Tailscale policy. Peer selection does not install an agent, reserve a seat or prove protection; registration enforces the current limit again.

Doctor reports gaps honestly. The hosted ThreatWarden reporting/enrollment routes must be deployed and tenant-scoped before the customer coverage flow is operational. A Pro entitlement alone is not proof that every security engine is working.

## Packaging

Build `ramwarden-cli` alongside the daemon and account helper. The Mac DMG embeds it and installs the `ramwarden-doctor` alias. Windows setup copies `ramwarden.exe` and the doctor alias. Existing settings, account tokens and history remain in place. The operator's repository environment is never copied into client installations.
