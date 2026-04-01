"""Tests for pipeline watcher callbacks."""

import cognee_pipeline as cp


def test_watcher_on_pipeline(ctx):
    """Watcher receives on_pipeline events."""
    events = []

    class MyWatcher:
        def on_pipeline(self, pipeline_id, status):
            events.append(("pipeline", status))

        def on_task(self, pipeline_id, task_index, name, total, status):
            events.append(("task", name, status))

    def identity(x):
        return x

    p = cp.Pipeline("watched")
    p.add_task(identity, name="identity")

    p.execute_sync([1], ctx, watcher=MyWatcher())

    # Should have at least started and succeeded events.
    pipeline_events = [e for e in events if e[0] == "pipeline"]
    assert len(pipeline_events) >= 2  # started + succeeded

    task_events = [e for e in events if e[0] == "task"]
    assert len(task_events) >= 1


def test_watcher_missing_methods(ctx):
    """Watcher with missing methods should not crash."""

    class PartialWatcher:
        def on_pipeline(self, pipeline_id, status):
            pass
        # Intentionally missing on_task

    def identity(x):
        return x

    p = cp.Pipeline("partial-watcher")
    p.add_task(identity, name="id")

    # Should not raise even though on_task is missing.
    results = p.execute_sync([42], ctx, watcher=PartialWatcher())
    assert results == [42]
