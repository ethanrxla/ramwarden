import os
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class ThresholdConfig:
    ram_percent: float = 75.0    # warn level — heuristic kicks in (no API)
    critical_percent: float = 85.0  # critical level — Claude API kicks in
    inactivity_minutes: int = 60  # tabs inactive this long are candidates
    debounce_minutes: int = 15    # minimum gap between auto-triggers


@dataclass
class ServerConfig:
    port: int = 7823
    host: str = "0.0.0.0"  # bind all interfaces so Tailscale devices can connect


@dataclass
class Config:
    thresholds: ThresholdConfig = field(default_factory=ThresholdConfig)
    server: ServerConfig = field(default_factory=ServerConfig)
    watchlist: list[str] = field(default_factory=lambda: [
        "Discord", "BurpSuiteCommunity", "burpsuite", "cursor", "Cursor"
    ])
    workspace_rules: list[dict] = field(default_factory=list)
    anthropic_api_key: str = ""
    db_path: str = str(Path.home() / ".local" / "share" / "ramwarden" / "history.db")

    @property
    def critical_percent(self) -> float:
        return self.thresholds.critical_percent


_config: Config | None = None


def load(path: str | Path | None = None) -> Config:
    global _config

    if path is None:
        candidates = [
            Path.cwd() / "ramwarden.toml",
            Path.home() / ".config" / "ramwarden" / "config.toml",
        ]
        path = next((p for p in candidates if p.exists()), None)

    raw: dict = {}
    if path and Path(path).exists():
        with open(path, "rb") as f:
            raw = tomllib.load(f)

    t = raw.get("thresholds", {})
    s = raw.get("server", {})
    proc = raw.get("processes", {})

    _config = Config(
        thresholds=ThresholdConfig(
            ram_percent=t.get("ram_percent", 75.0),
            critical_percent=t.get("critical_percent", 85.0),
            inactivity_minutes=t.get("inactivity_minutes", 60),
            debounce_minutes=t.get("debounce_minutes", 15),
        ),
        server=ServerConfig(
            port=s.get("port", 7823),
            host=s.get("host", "127.0.0.1"),
        ),
        watchlist=proc.get("watchlist", [
            "Discord", "BurpSuiteCommunity", "burpsuite", "cursor", "Cursor"
        ]),
        workspace_rules=raw.get("workspaces", {}).get("rules", []),
        anthropic_api_key=os.environ.get("ANTHROPIC_API_KEY", raw.get("api", {}).get("key", "")),
        db_path=raw.get("db_path", str(Path.home() / ".local" / "share" / "ramwarden" / "history.db")),
    )
    return _config


def get() -> Config:
    if _config is None:
        return load()
    return _config
