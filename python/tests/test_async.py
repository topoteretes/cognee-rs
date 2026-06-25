"""Tests for async pipeline execution."""

import asyncio
import pytest
import cognee_py as cp


@pytest.mark.asyncio
async def test_async_execute_sync_task():
    """Async execute with a sync task."""
    ctx = cp.TaskContext.mock()

    def double(x):
        return x * 2

    p = cp.Pipeline("async-sync")
    p.add_task(double, name="double")

    results = await p.execute([5], ctx)
    assert results == [10]


@pytest.mark.asyncio
async def test_async_execute_async_task():
    """Async execute with an async task."""
    ctx = cp.TaskContext.mock()

    async def async_double(x):
        await asyncio.sleep(0.01)
        return x * 2

    p = cp.Pipeline("async-async")
    p.add_task(async_double, name="double")

    results = await p.execute([7], ctx)
    assert results == [14]


@pytest.mark.asyncio
async def test_async_chain_mixed():
    """Chain sync and async tasks together."""
    ctx = cp.TaskContext.mock()

    def add_one(x):
        return x + 1

    async def times_two(x):
        return x * 2

    p = cp.Pipeline("mixed")
    p.add_task(add_one, name="add")
    p.add_task(times_two, name="mul")

    # (3 + 1) * 2 = 8
    results = await p.execute([3], ctx)
    assert results == [8]


@pytest.mark.asyncio
async def test_async_generator_task():
    """Async generator function -> AsyncStream task."""
    ctx = cp.TaskContext.mock()

    async def async_split(text):
        for word in text.split():
            yield word

    p = cp.Pipeline("async-gen")
    p.add_task(async_split, name="split")

    results = await p.execute(["foo bar baz"], ctx)
    assert results == ["foo", "bar", "baz"]


@pytest.mark.asyncio
async def test_execute_in_background():
    """Execute in background and await the handle."""
    ctx = cp.TaskContext.mock()

    def inc(x):
        return x + 1

    p = cp.Pipeline("bg")
    p.add_task(inc, name="inc")

    handle = p.execute_in_background([10, 20], ctx)
    results = await handle.wait()
    assert sorted(results) == [11, 21]
