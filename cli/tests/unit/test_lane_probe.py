"""Unit tests for the lane_probe module (cli/src/fno/evals/lane_probe.py).

The module lives outside the tests/py tree, so the test resolves the repo
root from its own path (same parents[3] walk the tests/py suite uses) and
imports through the fully-qualified cli.src.fno... namespace chain, which
cannot collide with any installed fno package.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[3]
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))

from cli.src.fno.evals.lane_probe import lane_probe


def test_positive_input_doubles() -> None:
    """A positive input doubles correctly."""
    assert lane_probe(7) == 14


def test_zero_returns_zero() -> None:
    """Zero returns zero."""
    assert lane_probe(0) == 0


def test_negative_input_raises() -> None:
    """A negative input raises ValueError."""
    with pytest.raises(ValueError, match="non-negative"):
        lane_probe(-3)
