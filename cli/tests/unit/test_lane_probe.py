"""Unit tests for the lane_probe doubling probe."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# The module lives in the src layout and no package installs it here.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "src"))

from fno.evals.lane_probe import lane_probe


def test_positive_input_doubles() -> None:
    assert lane_probe(21) == 42


def test_zero_returns_zero() -> None:
    assert lane_probe(0) == 0


def test_negative_input_raises_value_error() -> None:
    with pytest.raises(ValueError):
        lane_probe(-3)
