#!/usr/bin/env python3
"""Read-only onboarding evidence. Never outputs tokens, alerts or browsing history."""
import argparse
import datetime
import json
import os
import pathlib
import platform
import shutil
import subprocess
import urllib.error
import urllib.request


FIELDS = {
    "health": ("ok", "version", "warm", "uptime_seconds"),
    "intel": ("enabled", "configured", "calls_last_hour", "max_calls_per_hour",
              "input_tokens", "output_tokens", "paused_for_seconds"),
}


def project(endpoint, data):
    if not isinstance(data, dict):
        return {"state": "invalid_response"}
    if endpoint in FIELDS:
        return {k: data[k] for k in FIELDS[endpoint] if k in data}
    status = data.get("status") or {}
    report = data.get("report") or {}
    engines = report.get("engines") or [] if isinstance(report, dict) else []
    return {
        "unlocked": data.get("unlocked"),
        "enabled": data.get("enabled"),
        "level": status.get("level") if isinstance(status, dict) else None,
        "engines": [{k: e[k] for k in ("id", "state") if k in e}
                    for e in engines if isinstance(e, dict)],
    }


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def probe(endpoint, port):
    try:
        # Only the local daemon is queried; never follow a redirect off-machine.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open("http://127.0.0.1:%d/%s" % (port, endpoint), timeout=5) as response:
            body = response.read(1_048_577)
            if len(body) > 1_048_576:
                return {"state": "oversized_response"}
            return project(endpoint, json.loads(body))
    except urllib.error.HTTPError as error:
        return {"state": "http_error", "status": error.code}
    except (OSError, ValueError):
        return {"state": "unavailable"}


def tailscale():
    candidates = [shutil.which("tailscale"),
                  "C:/Program Files/Tailscale/tailscale.exe",
                  "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
    executable = next((p for p in candidates if p and pathlib.Path(p).is_file()), None)
    if not executable:
        return {"state": "not_found"}
    try:
        result = subprocess.run([executable, "status", "--json"], capture_output=True,
                                text=True, timeout=10, check=True)
        data = json.loads(result.stdout)
        own = data.get("Self") or {}
        peers = (data.get("Peer") or {}).values()
        return {"state": data.get("BackendState"), "hostname": own.get("HostName"),
                "dns_name": own.get("DNSName"), "online": own.get("Online"),
                "online_peer_count": sum(p.get("Online") is True for p in peers)}
    except (OSError, ValueError, subprocess.SubprocessError):
        return {"state": "unavailable"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=7823)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("port must be between 1 and 65535")
    config = pathlib.Path(os.environ.get("APPDATA", str(pathlib.Path.home() / ".config"))) / "ramwarden"
    result = {"observed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "platform": platform.system(), "hostname": platform.node(),
              "device_token_file_present": (config / "device-token").is_file(),
              "tailscale": tailscale()}
    for endpoint in ("health", "intel", "wardens/threat"):
        result[endpoint] = probe(endpoint, args.port)
    result["limitations"] = [
        "A token file is not proof of a valid entitlement or account ownership.",
        "Running processes are not proof of manager enrollment or delivered alerts.",
        "calls_last_hour is a rolling window, not a daily count; token counters reset with the daemon.",
        "No plan quotas or settings were changed.",
    ]
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
