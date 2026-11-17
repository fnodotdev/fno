"""Capability-lane probe module: one doubled-value function."""


def lane_probe(n: int) -> int:
    """Return n doubled. Raise ValueError for a negative n."""
    if n < 0:
        raise ValueError(f"n must be non-negative, got {n}")
    return n * 2
