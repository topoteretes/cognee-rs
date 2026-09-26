# lbug: `checkpointNoLock` releases its exclusivity guard before it checkpoints

**Status:** written up for sending on; **not filed upstream** — see *Where to send
this* at the bottom.
**Affects:** `lbug` 0.14.1 (crates.io), vendored C++ at `lbug-src/`. The same
code shape exists in the Kùzu lineage this is forked from, so it is worth
checking there too.
**Found by:** cognee-rs, while root-causing an on-device graph that came back
empty after an app restart. That root cause turned out to be separate (nothing
in an embedded deployment ever reached a checkpoint at all); this defect was
found while reading the checkpoint path and is reported on its own merits.

## The defect

`lbug-src/src/transaction/transaction_manager.cpp`:

```cpp
void TransactionManager::checkpointNoLock(main::ClientContext& clientContext) {
    // Note: It is enough to stop and wait for transactions to leave the system instead of, for
    // example, checking on the query processor's task scheduler. ...
    try {
        auto lockForStartingTransaction = stopNewTransactionsAndWaitUntilAllTransactionsLeave();
    } catch (std::exception& e) {
        throw CheckpointException{e};
    }
    auto checkpointer = initCheckpointerFunc(clientContext);
    try {
        checkpointer->writeCheckpoint();
    } catch (std::exception& e) {
        checkpointer->rollback();
        throw CheckpointException{e};
    }
}
```

`stopNewTransactionsAndWaitUntilAllTransactionsLeave()` returns a `UniqLock`
holding `mtxForStartingNewTransactions`. That lock is what keeps new
transactions out for the duration of the checkpoint.

It is bound to `lockForStartingTransaction`, which is **a local of the `try`
block**. Its scope ends at the closing brace of that `try`, so the lock is
released immediately — before `initCheckpointerFunc` is called and before
`writeCheckpoint()` does any work.

The net effect is that the function waits for the system to drain, drops the
guarantee it just acquired, and then checkpoints with new transactions free to
start. The wait still happens, so the window is smaller than "no locking at
all", but the exclusivity the checkpoint is written against does not hold for
any of the time it is actually needed.

The comment directly above says what the author intended:

> It is enough to stop and wait for transactions to leave the system instead of,
> for example, checking on the query processor's task scheduler. This is because
> the first and last steps that a connection performs when executing a query are
> to start and commit/rollback transaction.

That reasoning is sound and describes a lock held *across* the checkpoint. The
code does not hold it.

## Why it matters

`Checkpointer::writeCheckpoint()` is not idempotent bookkeeping — it rewrites
the catalog and storage metadata, writes the database header, applies shadow
pages over the data file, and then ends with:

```cpp
mainStorageManager->getWAL().reset();     // unlinks <db>.wal
mainStorageManager->getShadowFile().reset();
```

So the tail of a checkpoint **deletes the write-ahead log**. If a transaction
commits into that WAL while the checkpoint is running — which this defect
permits — its records can be unlinked without having been folded into the data
file. For an embedded database whose data file has never been checkpointed, the
WAL is the only copy of everything, so the blast radius is not "the last few
writes" but "the entire database".

## Repro

I have **no deterministic reproduction of the corruption**, and I want to be
clear about that: the defect below is established by reading the code, not by
observing a torn checkpoint. What I can reproduce is the adjacent symptom that
the missing lock makes reachable — a checkpoint that cannot get exclusivity and
takes the calling write down with it.

Against `lbug` 0.14.1, writing through a graph adapter while three tasks issue
concurrent point reads, with enough volume to cross the 16 MB
`checkpoint_threshold` and trigger the auto-checkpoint on commit:

```
batch 0: total=150 main=4096 wal=3356467
batch 1: total=250 main=4096 wal=6686685
batch 2: total=350 main=4096 wal=10016903
batch 3: total=450 main=4096 wal=13347121
batch 4: total=550 main=4096 wal=16677339
thread panicked:
  Failed to batch-upsert 100 nodes: Query execution failed: Timeout waiting for
  active transactions to leave the system before checkpointing. If you have an
  open transaction, please close it and try again.
```

The write that tripped the threshold fails, rather than the checkpoint being
deferred — a committing writer is made to fail by the presence of unrelated
*readers*. `DEFAULT_CHECKPOINT_WAIT_TIMEOUT_IN_MICROS` is 5 s
(`src/include/common/constants.h:21`), and `canAutoCheckpoint` re-fires on the
next commit, so a read-heavy workload above the threshold can stall repeatedly.

That behaviour is arguably by design. It is included here because it is the
observable half of the same area, and because a reviewer fixing the scope bug
will want to decide deliberately whether an auto-checkpoint should be able to
fail a user's write at all, or should simply skip and retry later.

## Suggested fix

Hold the lock for the whole checkpoint by giving it the function's scope:

```cpp
void TransactionManager::checkpointNoLock(main::ClientContext& clientContext) {
    UniqLock lockForStartingTransaction = [&] {
        try {
            return stopNewTransactionsAndWaitUntilAllTransactionsLeave();
        } catch (std::exception& e) {
            throw CheckpointException{e};
        }
    }();

    auto checkpointer = initCheckpointerFunc(clientContext);
    try {
        checkpointer->writeCheckpoint();
    } catch (std::exception& e) {
        checkpointer->rollback();
        throw CheckpointException{e};
    }
    // lock released here, after the WAL has been reset
}
```

`UniqLock` must be movable for this (it is a move-only lock wrapper, so the
immediately-invoked-lambda form works); if it is not, hoist a
`std::optional<UniqLock>` declared before the `try` and `emplace` into it.

Worth adding alongside the fix, since nothing currently detects a regression of
this shape:

- an assertion in `Checkpointer::writeCheckpoint()` that
  `mtxForStartingNewTransactions` is held, and
- a test that starts a transaction from another thread while a checkpoint is in
  progress and asserts it blocks until the checkpoint completes.

A local `std::lock_guard`/`UniqLock` whose only use is its destructor is easy to
scope wrongly and invisible once written; `[[nodiscard]]` on
`stopNewTransactionsAndWaitUntilAllTransactionsLeave()` would not have caught
this one (the value *is* used — bound to a local), but a clang-tidy rule against
lock guards whose scope ends before the operation they protect would.

## Where to send this

Not filed. `lbug` 0.14.1's manifest declares
`repository = "https://github.com/lbugdb/lbug"`, and that repository does not
resolve for us — it is private, renamed, or gone (`gh repo view lbugdb/lbug` →
`Could not resolve to a Repository`). It is also not a topoteretes repository,
so it is not ours to open an issue on unilaterally.

If someone has the right destination — a vendor contact, a private mirror, or a
topoteretes fork of lbug — this file is self-contained and can be pasted as an
issue as-is. If the decision is instead to carry a local patch, the fix above is
three lines against `lbug-src/src/transaction/transaction_manager.cpp` and would
need the vendored C++ to be patched at build time, which the `lbug` crate's
`build.rs` does not currently support.
