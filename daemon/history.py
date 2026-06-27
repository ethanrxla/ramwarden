import sqlite3
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from . import config


@dataclass
class ClosedTab:
    id: int
    url: str
    title: str
    closed_at: str
    ram_freed_mb: float
    trigger_type: str   # "auto" | "manual"
    goal_context: str = ""


def _conn() -> sqlite3.Connection:
    db_path = Path(config.get().db_path)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    return conn


def init_db():
    with _conn() as conn:
        conn.execute("""
            CREATE TABLE IF NOT EXISTS closed_tabs (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                url          TEXT NOT NULL,
                title        TEXT NOT NULL,
                closed_at    TEXT NOT NULL,
                ram_freed_mb REAL NOT NULL DEFAULT 0,
                trigger_type TEXT NOT NULL DEFAULT 'auto',
                goal_context TEXT NOT NULL DEFAULT ''
            )
        """)
        # Migration: add goal_context to existing databases without it
        try:
            conn.execute("ALTER TABLE closed_tabs ADD COLUMN goal_context TEXT NOT NULL DEFAULT ''")
        except Exception:
            pass  # column already exists — no-op


def save_tabs(
    tabs: list[dict],
    ram_freed_mb: float = 0.0,
    trigger_type: str = "auto",
    goal_context: str = "",
):
    """tabs: list of {url, title} dicts"""
    now = datetime.utcnow().isoformat()
    per_tab_ram = round(ram_freed_mb / len(tabs), 1) if tabs else 0.0
    with _conn() as conn:
        conn.executemany(
            """INSERT INTO closed_tabs
               (url, title, closed_at, ram_freed_mb, trigger_type, goal_context)
               VALUES (?,?,?,?,?,?)""",
            [
                (t["url"], t.get("title", t["url"]), now, per_tab_ram, trigger_type, goal_context)
                for t in tabs
            ],
        )


def recent(limit: int = 50) -> list[ClosedTab]:
    with _conn() as conn:
        rows = conn.execute(
            "SELECT * FROM closed_tabs ORDER BY closed_at DESC LIMIT ?", (limit,)
        ).fetchall()
    return [ClosedTab(**dict(r)) for r in rows]


def clear():
    with _conn() as conn:
        conn.execute("DELETE FROM closed_tabs")
