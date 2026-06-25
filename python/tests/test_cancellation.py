"""Tests for cancellation and progress tracking."""

import cognee_py as cp


def test_cancellation_handle():
    ctx = cp.TaskContext.mock()
    h = ctx.cancellation_handle
    assert not h.is_cancelled
    h.cancel()
    assert h.is_cancelled


def test_cancellation_pair():
    handle, token = cp.cancellation_pair()
    assert not token.is_cancelled
    handle.cancel()
    assert token.is_cancelled


def test_cancellation_token_clone():
    handle, token = cp.cancellation_pair()
    token2 = token.clone_token()
    assert not token2.is_cancelled
    handle.cancel()
    assert token2.is_cancelled


def test_cancellation_pair_returns_correct_types():
    handle, token = cp.cancellation_pair()
    assert isinstance(handle, cp.CancellationHandle)
    assert isinstance(token, cp.CancellationToken)


def test_progress_token():
    pt = cp.ProgressToken()
    assert pt.fraction == 0.0
    assert not pt.is_complete

    pt.set(0.5)
    assert abs(pt.fraction - 0.5) < 1e-10
    assert abs(pt.root_fraction - 0.5) < 1e-10

    pt.set(1.0)
    assert pt.is_complete


def test_progress_split():
    pt = cp.ProgressToken()
    subs = pt.split([1, 2, 1])
    assert len(subs) == 3

    subs[0].set(1.0)
    subs[1].set(0.5)
    subs[2].set(0.0)

    # root = 0.25*1.0 + 0.5*0.5 + 0.25*0.0 = 0.5
    assert abs(pt.root_fraction - 0.5) < 1e-10


def test_context_progress():
    ctx = cp.TaskContext.mock()
    pt = ctx.progress
    assert pt.fraction == 0.0
    pt.set(0.75)
    assert abs(pt.fraction - 0.75) < 1e-10
