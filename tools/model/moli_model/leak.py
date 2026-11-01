"""A last check that nothing personal slipped into the data."""
import re

PATTERNS = {
    "adresse IPv4": re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
    "adresse MAC": re.compile(r"\b(?:[0-9a-fA-F]{2}[:-]){5}[0-9a-fA-F]{2}\b"),
    "e-mail": re.compile(r"[\w.+-]+@[\w-]+\.[\w.-]+"),
    "clé ou jeton": re.compile(r"\b(?:sk|pk|ghp|xox[bp])[-_][A-Za-z0-9_-]{12,}\b"),
}


def scan(text):
    """Names of the patterns found in `text`."""
    return [name for name, pattern in PATTERNS.items() if pattern.search(text)]
