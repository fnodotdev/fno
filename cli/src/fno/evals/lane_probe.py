"""Capability-lane probe: the smallest callable the lane can ship."""
from __future__ import annotations


def lane_probe(n: int) -> int:
    """Return n doubled. Raise ValueError for a negative n."""
    if n < 0:
        raise ValueError(f"lane_probe expects a non-negative n, got {n}")
    return n * 2
