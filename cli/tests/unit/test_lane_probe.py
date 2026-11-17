"""Tests for cli/src/fno/evals/lane_probe.py.

The probe doubles a non-negative int and refuses a negative one. Three
contracts, one test each: a positive input doubles, zero stays zero, and a
negative input raises ValueError.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
# cli/ is not an installed package (no pyproject.toml), so the import needs
# the source tree on sys.path; this file is outside tests/py and its
# conftest, so it bootstraps itself.
sys.path.insert(0, str(REPO / "cli" / "src"))

from fno.evals.lane_probe import lane_probe  # noqa: E402 - path bootstrap above


def test_positive_input_doubles():
    assert lane_probe(21) == 42


def test_zero_returns_zero():
    assert lane_probe(0) == 0


def test_negative_input_raises_value_error():
    with pytest.raises(ValueError):
        lane_probe(-7)
