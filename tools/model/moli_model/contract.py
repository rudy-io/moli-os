"""The frozen contract: tools (and version) shared with Moli's Rust side."""
import json
from pathlib import Path

CONTRACT_DIR = Path(__file__).resolve().parents[1] / "contract"


def load():
    """tools.json as a dict: {"version": int, "tools": [...]}."""
    return json.loads((CONTRACT_DIR / "tools.json").read_text(encoding="utf-8"))
