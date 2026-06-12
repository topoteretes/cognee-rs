"""Tests for the Watcher factory class."""

import cognee_pipeline as cp


def test_watcher_noop_does_not_raise():
    w = cp.Watcher.noop()
    # All event methods should be callable without error.
    w.on_pipeline("pid", "started")
    w.on_task("pid", 0, "task", 1, "running")
    w.on_run_started("rid", "pipeline")
    w.on_run_completed("rid", 3)
    w.on_run_errored("rid", "oh no")
    w.on_task_started("rid", "task", 0)
    w.on_task_completed("rid", "task", 2)
    w.on_task_errored("rid", "task", "boom")


def test_watcher_callback_fires(ctx):
    received = []
    w = cp.Watcher(on_task_started=lambda run_id, name, idx: received.append((run_id, name, idx)))

    def identity(x):
        return x

    p = cp.Pipeline("watcher-factory-test")
    p.add_task(identity, name="identity")
    p.execute_sync([42], ctx, watcher=w)

    # At least one on_task_started event should have fired.
    assert len(received) >= 1


def test_watcher_missing_callback_ignored(ctx):
    # Watcher with only one callback — others are silently ignored.
    fired = []
    w = cp.Watcher(on_run_completed=lambda run_id, count: fired.append(count))

    def identity(x):
        return x

    p = cp.Pipeline("watcher-partial-test")
    p.add_task(identity, name="identity")
    results = p.execute_sync([1, 2, 3], ctx, watcher=w)
    assert results == [1, 2, 3]
    # on_run_completed should have fired once.
    assert len(fired) >= 1


def test_watcher_is_in_all():
    assert "Watcher" in cp.__all__


def test_watcher_default_constructor():
    w = cp.Watcher()
    # All methods callable, no callbacks registered.
    w.on_pipeline("p", "ok")
    w.on_run_started("r", "pipeline")
