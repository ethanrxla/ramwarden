# Hosting ThreatWarden (operator runbook)

Users never run a server: RamWarden hosts Wazuh for every ThreatWarden pass.
This is the one-time job for the operator. It is sized for launch: one
**Oracle Cloud Always Free** ARM server, $0 a month. Move to a paid server
when passes grow (see "Scaling" below). The behaviour this supports is in
[specs/wardens.md](specs/wardens.md) and [specs/threatwarden.md](specs/threatwarden.md).

## 1. Create the server (Oracle Cloud console, ~10 minutes)

1. Sign up at cloud.oracle.com on Free Tier. Do not upgrade billing as part of
   this setup. Confirm the resources are marked Always Free eligible in the
   console; availability and account-wide quotas still apply. Oracle may reclaim
   idle Always Free instances, so retain backups and plan for recovery.
2. **Compute → Instances → Create instance**:
   - Image: **Canonical Ubuntu 24.04** (aarch64).
   - Shape: **VM.Standard.A1.Flex**, **2 OCPU, 12 GB** (the documented account-wide A1 Always Free allowance).
     If it says "out of host capacity", try another availability domain or
     try again later; free ARM capacity comes and goes.
   - Boot volume: **100 GB** (free up to 200 GB total).
   - Add your SSH public key.
3. **Networking → the instance's subnet → Security list → Add ingress rule**:
   source `0.0.0.0/0`, TCP, destination port **1514**. Nothing else: the
   manager API, indexer and dashboard stay closed to the internet.
4. Note the instance's **public IP**. In your DNS, create an **A record**
   `agents.ramwarden.app` → that IP. Agents connect to this name, so the
   server can move later without touching any user's computer.

## 2. Install Wazuh (one command, ~20 minutes)

```bash
scp scripts/threatwarden-server/setup.sh ubuntu@agents.ramwarden.app:
ssh ubuntu@agents.ramwarden.app 'sudo bash setup.sh'
```

It enforces IPv4/IPv6 management-port filtering before service startup, adds swap, installs Wazuh's official all-in-one (manager, indexer,
dashboard), turns off password enrolment (each agent gets its own key from
ramwarden-api instead), and opens only port 1514 in the host firewall. At the
end it prints where the generated passwords are.

## 3. Connect ramwarden-api

ramwarden-api runs on the same server (it is small), so the manager API and
indexer are reached on `127.0.0.1` and never exposed. Give it, as environment
variables (never in a file in git):

| Variable | Value |
|---|---|
| `WAZUH_MANAGER_URL` | `https://127.0.0.1:55000` |
| `WAZUH_API_USER` / `WAZUH_API_PASSWORD` | `wazuh-wui` and its password from the passwords file |
| `WAZUH_INDEXER_URL` | `https://127.0.0.1:9200` |
| `WAZUH_INDEXER_USER` / `WAZUH_INDEXER_PASSWORD` | `admin` and its password |
| `WAZUH_CA_FILE` | `/etc/wazuh-indexer/certs/root-ca.pem` |
| `WAZUH_AGENT_HOST` | `agents.ramwarden.app` |

## 4. Check it

- `sudo systemctl status wazuh-manager wazuh-indexer` are both active.
- From your computer: `nc -vz agents.ramwarden.app 1514` connects, and
  `nc -vz agents.ramwarden.app 55000` does **not**.
- The dashboard, for you only: `ssh -L 8443:127.0.0.1:443 ubuntu@agents.ramwarden.app`,
  then open `https://localhost:8443` (user `admin`).
- Buy a ThreatWarden pass on a test account, sign in on a test computer, click
  **Turn on**, approve the prompt, and the agent shows as active in the
  dashboard under group `acct-…`.

## Scaling

Wazuh's own sizing is 4 vCPU / 8 GB for 1-25 agents and 8 vCPU / 8 GB for up
to 100, so this free ARM box is a launch server; benchmark the real workload. Signs it is time to move:
the indexer's disk above 70%, the manager's queue warnings in
`/var/ossec/logs/ossec.log`, or more than ~25 active agents. Moving is: build
the new server with the same script, restore the manager's `/var/ossec/etc`
(agent keys and groups), point the `agents.ramwarden.app` A record at it.
Agents reconnect on their own.

## Backups

The agent keys and groups live in `/var/ossec/etc` (`client.keys`,
`shared/`). Back that directory up daily (Oracle's free block-volume backups
cover it). Alerts are history and can be lost without breaking anything.

The script reserves port 443 for the private Wazuh dashboard. Deploying the public
RamWarden API needs a separate HTTPS ingress configuration; do not expose the
dashboard by simply removing that firewall rule. Hosted enroll/status routes are
still required before the customer one-click flow can work.
