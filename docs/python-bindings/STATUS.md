# Python Bindings Implementation — Status

Tracking file for the task plan in [IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md).
Updated by the Phase-4 finalizer agent after each task. Do not edit rows for tasks that are
not yet finished.

Status values: `pending` | `in-progress` | `done` | `blocked` | `skipped`

| ID | Title | Plan document | Status | Date | Commit |
|----|-------|---------------|--------|------|--------|
| T1 | Python SDK handle + error hierarchy | [sdk-handle.md](sdk-handle.md) | done | 2026-06-12 | e69329e |
| T2 | Config surface | [config-surface.md](config-surface.md) | done | 2026-06-12 | b1ba3dd |
| T3 | Hoist pipeline ops + Python add/cognify/add_and_cognify | [core-pipeline-ops.md](core-pipeline-ops.md) | done | 2026-06-12 | T3a: 2581cdd, T3b: 62ee0bb |
| T4 | Hoist + Python search/recall | [retrieval-ops.md](retrieval-ops.md) | done | 2026-06-12 | 566d97e |
| T5 | Hoist + Python forget/update/prune | [data-ops.md](data-ops.md) | done | 2026-06-12 | c9813d4 |
| T6 | Hoist + Python dataset management | [dataset-management.md](dataset-management.md) | done | 2026-06-12 | 2632cc8 |
| T7 | Hoist + Python remember/memify/improve | [memory-ops.md](memory-ops.md) | done | 2026-06-12 | 79b7a70 |
| T8 | Hoist + Python sessions/admin/notebooks | [session-admin-ops.md](session-admin-ops.md) | done | 2026-06-12 | 8c7ab8d |
| T9 | Python visualization ops | [visualization-ops.md](visualization-ops.md) | done | 2026-06-12 | — |
| T10 | Python cloud serve/disconnect | [cloud-ops.md](cloud-ops.md) | pending | — | — |
| T11 | Minor engine-tier gaps | [minor-engine-gaps.md](minor-engine-gaps.md) | pending | — | — |

## Log

Free-form notes appended by agents (task splits, deferrals, blockers encountered and how they
were resolved). Newest entries last.

- T3 was split into T3a (hoist+rewire, done) and T3b (Python surface, pending). T3a completed 2026-06-12.
- T3b completed 2026-06-12: add/cognify/add_and_cognify async methods on PyCognee in python/src/sdk_ops.rs, test isolation via relational_db_url key, ConfigManager::set() relational_db_url support, ComponentManager::init_database() SQLite directory creation. All 81 Python tests pass.
