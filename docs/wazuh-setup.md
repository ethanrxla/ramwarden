# Setting up the Wazuh warden

The Wazuh warden reports what this computer's Wazuh security agent has seen.
RamWarden does not ship or run Wazuh itself: you install Wazuh's own packages,
and RamWarden reads from them. This page is everything that has to be done by
hand. The behaviour is specified in [specs/wardens.md](specs/wardens.md).

There are three pieces:

| Piece | Where it runs | What it is |
|---|---|---|
| **Wazuh central components** (manager, indexer, dashboard) | One always-on 64-bit Linux server | Collects events from agents, runs detection rules, stores alerts |
| **Wazuh agent** | Every computer RamWarden runs on | Watches logs, files and config; sends events to the manager |
| **RamWarden** | Every computer | Reads the agent's state and asks the manager and indexer for alerts |

## 1. The server (once)

Needs a 64-bit Linux host (Ubuntu 22.04/24.04, RHEL 8-10, Amazon Linux 2023)
with **4 vCPU, 8 GB RAM and 50 GB disk** for up to 25 computers. A small cloud
VM or a spare machine on your LAN both work. It must not be one of the
computers RamWarden is trying to save memory on: the indexer alone wants
several GB of RAM.

```bash
curl -sO https://packages.wazuh.com/4.14/wazuh-install.sh
sudo bash ./wazuh-install.sh -a
```

When it finishes, keep two files from the directory you ran it in:

- `wazuh-install-files.tar`: the passwords. Print them with
  `sudo tar -O -xvf wazuh-install-files.tar wazuh-install-files/wazuh-passwords.txt`.
- `wazuh-certificates.tar`: contains `root-ca.pem`. RamWarden needs this to
  trust the server's self-signed certificate.

Open these ports to the computers that will use it (and nothing to the open
internet that you don't need):

| Port | For |
|---|---|
| 1514/tcp | Agents sending events |
| 1515/tcp | Agents enrolling |
| 55000/tcp | Manager API (RamWarden reads agent health) |
| 9200/tcp | Indexer (RamWarden reads alerts) |
| 443/tcp | Dashboard, for you in a browser |

### Read-only accounts for RamWarden

Don't give RamWarden the admin passwords. In the dashboard
(`https://<server>`, user `admin`):

1. **Server management → Security → Users**: create `ramwarden-api` and give
   it the `readonly` role. This is the manager API user.
2. **Indexer management → Security → Roles**: create `ramwarden-alerts` with
   cluster permission `cluster_composite_ops_ro` and index permission `read`
   on `wazuh-alerts-*`. Then **Internal users**: create `ramwarden-alerts`
   and map it to that role. This is the indexer user.

## 2. The agent (on each computer)

Use Wazuh's own installer for each OS, pointing it at your server. The
dashboard's **Agents → Deploy new agent** page generates the exact command for
you; the Wazuh guides are:

- Linux: <https://documentation.wazuh.com/4.14/installation-guide/wazuh-agent/wazuh-agent-package-linux.html>
- Windows: <https://documentation.wazuh.com/4.14/installation-guide/wazuh-agent/wazuh-agent-package-windows.html>
- macOS: <https://documentation.wazuh.com/4.14/installation-guide/wazuh-agent/wazuh-agent-package-macos.html>

In each, set `WAZUH_MANAGER` to your server's address and, ideally,
`WAZUH_AGENT_NAME` to the computer's hostname (that is the name RamWarden
looks for unless you set `agent_name`). The agent uses about 30-60 MB of RAM.
RamWarden classifies every Wazuh process as a protected system process, so it
will never pause or close the agent to free memory.

Per-OS notes:

- **Linux**: the agent's state file is readable only by root and the `wazuh`
  group. Either add your user to that group
  (`sudo usermod -aG wazuh $USER`, then log out and in) or rely on the
  manager's view, which works without it.
- **macOS**: same as Linux; the agent lives in `/Library/Ossec`.
- **Windows**: the agent runs as the `WazuhSvc` service. Nothing extra needed.

## 3. RamWarden (on each computer)

The Wazuh warden is a Pro warden: it needs a Wazuh pass (on its own or in a
bundle). Once you have it, add this to RamWarden's config (`ramwarden.toml`, or
`%APPDATA%\ramwarden\config.toml` / `~/.config/ramwarden/config.toml`):

```toml
[wardens.wazuh]
enabled      = true
manager_url  = "https://wazuh.example.lan:55000"
api_user     = "ramwarden-api"
indexer_url  = "https://wazuh.example.lan:9200"
indexer_user = "ramwarden-alerts"
ca_file      = "/path/to/root-ca.pem"   # from wazuh-certificates.tar
# agent_name = "my-desktop"             # if it enrolled under another name
# min_level  = 7                         # ignore alerts below this rule level
# alert_level = 12                       # this level and up is shown as urgent
```

Put the passwords in `secrets.toml` beside it (never in `ramwarden.toml`,
which may be in git):

```toml
[wardens.wazuh]
api_password     = "..."
indexer_password = "..."
```

or in the environment as `WAZUH_API_PASSWORD` and `WAZUH_INDEXER_PASSWORD`
(a `.env` beside the config works too). Restart the daemon, then check:

```bash
curl -s http://127.0.0.1:7823/wardens/wazuh
```

`status.level` is `clear`, `watch` or `alert` when it is working. `off` comes
with a headline saying what is missing.

RamWarden refuses plain `http://` for the manager and indexer unless they are
on the same computer, and has no switch to skip certificate checks: use
`ca_file` for a self-signed server.

## Licence note

Wazuh is GPLv2 (its newer engine is AGPLv3). RamWarden only talks to it over
its files and network APIs and never ships modified Wazuh code, so RamWarden's
own licence and the paid add-on are unaffected. If RamWarden ever bundles the
Wazuh agent installer in its own installer, ship it unmodified and include
Wazuh's licence and a link to its source alongside it.
