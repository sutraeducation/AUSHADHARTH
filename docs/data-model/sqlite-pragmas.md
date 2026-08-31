# SQLite connection policy

The Store Service applies these settings to every SQLx SQLite connection:

| Setting | Value | Reason |
| --- | --- | --- |
| `foreign_keys` | `ON` | Enforce declared relational integrity per connection. |
| `journal_mode` | `WAL` | Support resilient transactions and concurrent readers. |
| `busy_timeout` | `5000 ms` | Wait briefly for expected writer contention instead of failing immediately. |
| `synchronous` | `FULL` | Favor committed-data durability across power loss. |

The database is created if absent and forward-only embedded migrations run before
the HTTP listener accepts requests. Business write use cases must declare an
explicit transaction boundary. SQLite is never opened over a network share.
