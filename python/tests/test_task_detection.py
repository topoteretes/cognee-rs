"""Tests that verify callable type auto-detection works correctly.

We verify indirectly by checking pipeline behavior: sync functions produce
single outputs, generators produce multiple outputs, async functions work
with await, etc.
"""

import asyncio
import pytest
import cognee_py as cp


def test_detects_sync_function(ctx):
    def sync_fn(x):
        return x

    p = cp.Pipeline("detect-sync")
    p.add_task(sync_fn)
    results = p.execute_sync([42], ctx)
    assert results == [42]


def test_detects_generator(ctx):
    def gen_fn(x):
        yield x
        yield x + 1

    p = cp.Pipeline("detect-gen")
    p.add_task(gen_fn)
    results = p.execute_sync([10], ctx)
    assert results == [10, 11]


@pytest.mark.asyncio
async def test_detects_async_function():
    ctx = cp.TaskContext.mock()

    async def async_fn(x):
        return x + 100

    p = cp.Pipeline("detect-async")
    p.add_task(async_fn)
    results = await p.execute([1], ctx)
    assert results == [101]


@pytest.mark.asyncio
async def test_detects_async_generator():
    ctx = cp.TaskContext.mock()

    async def async_gen_fn(x):
        yield x
        yield x * 2

    p = cp.Pipeline("detect-async-gen")
    p.add_task(async_gen_fn)
    results = await p.execute([5], ctx)
    assert results == [5, 10]
