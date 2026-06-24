"""Tests for pipeline construction and synchronous execution."""

import cognee_py as cp


def test_pipeline_creation():
    p = cp.Pipeline("test pipeline")
    assert p is not None


def test_pipeline_with_name():
    p = cp.Pipeline("test")
    p.with_name("my-pipeline")


def test_pipeline_add_sync_task(ctx):
    def double(x):
        return x * 2

    p = cp.Pipeline("doubler")
    p.add_task(double, name="double")

    results = p.execute_sync([10], ctx)
    assert results == [20]


def test_pipeline_chain_two_tasks(ctx):
    def add_one(x):
        return x + 1

    def times_three(x):
        return x * 3

    p = cp.Pipeline("chain")
    p.add_task(add_one, name="add_one")
    p.add_task(times_three, name="times_three")

    # (10 + 1) * 3 = 33
    results = p.execute_sync([10], ctx)
    assert results == [33]


def test_pipeline_multiple_inputs(ctx):
    def square(x):
        return x * x

    p = cp.Pipeline("square")
    p.add_task(square, name="square")

    results = p.execute_sync([2, 3, 4], ctx)
    assert sorted(results) == [4, 9, 16]


def test_pipeline_string_processing(ctx):
    def upper(s):
        return s.upper()

    p = cp.Pipeline("upper")
    p.add_task(upper, name="upper")

    results = p.execute_sync(["hello", "world"], ctx)
    assert sorted(results) == ["HELLO", "WORLD"]


def test_pipeline_generator_task(ctx):
    """Generator function -> SyncIter task: one input produces multiple outputs."""

    def split_words(text):
        for word in text.split():
            yield word

    p = cp.Pipeline("splitter")
    p.add_task(split_words, name="split")

    results = p.execute_sync(["hello world"], ctx)
    assert results == ["hello", "world"]


def test_pipeline_generator_chain(ctx):
    """Generator followed by a regular task."""

    def split_words(text):
        for word in text.split():
            yield word

    def upper(s):
        return s.upper()

    p = cp.Pipeline("split-then-upper")
    p.add_task(split_words, name="split")
    p.add_task(upper, name="upper")

    results = p.execute_sync(["hello world"], ctx)
    assert sorted(results) == ["HELLO", "WORLD"]


def test_pipeline_retry(ctx):
    """Pipeline with retry policy."""
    call_count = 0

    def flaky(x):
        nonlocal call_count
        call_count += 1
        if call_count < 3:
            raise ValueError("not yet")
        return x

    p = cp.Pipeline("retry-test")
    p.add_task(flaky, name="flaky")
    p.with_retry(3, 10)

    results = p.execute_sync([42], ctx)
    assert results == [42]
    assert call_count == 3


def test_pipeline_no_tasks_error(ctx):
    """Empty pipeline raises NoTasksError."""
    p = cp.Pipeline("empty")

    try:
        p.execute_sync([1], ctx)
        assert False, "should have raised"
    except cp.NoTasksError:
        pass


def test_pipeline_dict_passthrough(ctx):
    """Arbitrary Python dicts can pass through the pipeline."""

    def add_key(d):
        d["processed"] = True
        return d

    p = cp.Pipeline("dict")
    p.add_task(add_key, name="add_key")

    results = p.execute_sync([{"name": "test"}], ctx)
    assert len(results) == 1
    assert results[0]["name"] == "test"
    assert results[0]["processed"] is True
