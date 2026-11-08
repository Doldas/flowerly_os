#!/usr/bin/env python3
"""Experimental OFFLINE agentic memory storage, not wired to flowerlyd (#86).

Actor owner MUST be obtained from the authenticated IPC boundary by a future
adapter. A caller-provided uid alone is not authorization.
"""

import json
import math
import sqlite3
from pathlib import Path


class InvalidEmbedding(ValueError):
    pass


def normalized(values, dimension):
    if not isinstance(values, (list, tuple)) or not 1 <= dimension <= 4096:
        raise InvalidEmbedding("invalid dimension")
    if len(values) != dimension or any(
        isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v)
        for v in values
    ):
        raise InvalidEmbedding("non-finite or wrong-size embedding")
    norm = math.sqrt(sum(float(v) ** 2 for v in values))
    if not norm or not math.isfinite(norm):
        raise InvalidEmbedding("zero or non-finite vector")
    return [float(v) / norm for v in values]


class OfflineMemory:
    """A separate, opt-in prototype SQLite file: NEVER open state.db."""

    def __init__(self, filename):
        path = Path(filename)
        if path.name == "state.db" or path.name == "system_state.db":
            raise ValueError("core database is not a memory backend")
        self.db = sqlite3.connect(path)
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.execute("""CREATE TABLE IF NOT EXISTS memories (
            id TEXT PRIMARY KEY, owner_uid INTEGER NOT NULL, content TEXT NOT NULL,
            model TEXT NOT NULL, version INTEGER NOT NULL, dimension INTEGER NOT NULL,
            vector TEXT NOT NULL, created_at INTEGER NOT NULL)""")
        self.db.execute(
            "CREATE INDEX IF NOT EXISTS memory_owner_model ON memories(owner_uid,model,version)"
        )

    def close(self):
        self.db.close()

    def put(self, actor_uid, memory_id, content, model, version, values, created_at):
        if (
            not isinstance(actor_uid, int)
            or isinstance(actor_uid, bool)
            or not 0 <= actor_uid <= 4294967294
        ):
            raise ValueError("invalid authenticated actor")
        if not memory_id or not model or version <= 0 or not isinstance(content, str):
            raise ValueError("invalid memory metadata")
        unit = normalized(values, len(values))
        with self.db:
            self.db.execute(
                "INSERT INTO memories VALUES(?,?,?,?,?,?,?,?)",
                (
                    memory_id,
                    actor_uid,
                    content,
                    model,
                    version,
                    len(unit),
                    json.dumps(unit, separators=(",", ":"), allow_nan=False),
                    created_at,
                ),
            )

    def search(self, actor_uid, model, version, values, limit=10):
        if not 1 <= limit <= 100:
            raise ValueError("invalid limit")
        unit = normalized(values, len(values))
        rows = self.db.execute(
            "SELECT id,vector,dimension FROM memories WHERE owner_uid=? AND model=? AND version=?",
            (actor_uid, model, version),
        ).fetchall()
        scores = []
        for memory_id, raw, dimension in rows:
            if dimension != len(unit):
                continue
            stored = normalized(json.loads(raw), dimension)
            scores.append((sum(x * y for x, y in zip(unit, stored)), memory_id))
        scores.sort(key=lambda entry: (-entry[0], entry[1]))
        return scores[:limit]

    def delete(self, actor_uid, memory_id):
        with self.db:
            return self.db.execute(
                "DELETE FROM memories WHERE owner_uid=? AND id=?",
                (actor_uid, memory_id),
            ).rowcount


if __name__ == "__main__":
    raise SystemExit(
        "offline prototype: import and run verified tests; no daemon integration"
    )
