"""Tests for ProgressToken.width and ProgressToken.subtoken()."""

import pytest

import cognee_pipeline as cp


def test_progress_width_root():
    root = cp.ProgressToken()
    assert root.width == 1.0


def test_subtoken_width():
    root = cp.ProgressToken()
    sub = root.subtoken(0.5)
    assert abs(sub.width - 0.5) < 1e-9


def test_subtoken_shrinks_parent():
    root = cp.ProgressToken()
    sub = root.subtoken(0.3)
    assert abs(sub.width - 0.3) < 1e-9
    assert abs(root.width - 0.7) < 1e-9


def test_subtoken_root_fraction():
    root = cp.ProgressToken()
    sub = root.subtoken(0.5)
    sub.set(1.0)
    # child covers 0.5, fully complete → root_fraction should be 0.5
    assert abs(root.root_fraction - 0.5) < 1e-9


def test_subtoken_invalid_frac_width_negative():
    root = cp.ProgressToken()
    with pytest.raises(ValueError):
        root.subtoken(-0.1)


def test_subtoken_invalid_frac_width_too_large():
    root = cp.ProgressToken()
    with pytest.raises(ValueError):
        root.subtoken(1.1)


def test_subtoken_boundary_zero():
    root = cp.ProgressToken()
    sub = root.subtoken(0.0)
    assert abs(sub.width) < 1e-9
    assert abs(root.width - 1.0) < 1e-9


def test_subtoken_boundary_one():
    root = cp.ProgressToken()
    sub = root.subtoken(1.0)
    assert abs(sub.width - 1.0) < 1e-9
    assert abs(root.width) < 1e-9
