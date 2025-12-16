# Optimistic Concurrency

When multiple processes or threads write to the same aggregate simultaneously, you risk
losing updates. Optimistic concurrency control detects these conflicts by checking that
the stream version hasn't changed between loading the aggregate and committing new events.

## Default Behavior

By default, repositories use **optimistic concurrency**—version checking is performed on every write. This is the safe default for production systems.

```rust,ignore
use event_sourcing::{Repository, store::{inmemory, JsonCodec}};

let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
let mut repo = Repository::new(store); // Optimistic concurrency enabled
```

The concurrency strategy is encoded in the type system, so you get compile-time guarantees about which error types you need to handle.

## Disabling Concurrency Checking

For single-writer scenarios where concurrency checking is unnecessary, you can opt out:

```rust,ignore
let mut repo = Repository::new(store)
    .without_concurrency_checking();
```

This returns a `Repository<S, Unchecked>` which uses last-writer-wins semantics.

## Error Types

The two concurrency strategies use different error types:

| Strategy | Error Type | Includes Concurrency Variant? |
|----------|------------|-------------------------------|
| `Optimistic` (default) | `OptimisticCommandError` | Yes |
| `Unchecked` | `CommandError` | No |

When using optimistic concurrency, `execute_command` returns
`OptimisticCommandError::Concurrency(conflict)` if the stream version changed between
loading and committing:

```rust,ignore
use event_sourcing::OptimisticCommandError;

match repo
    .execute_command::<MyAggregate, MyCommand>(&id, &command, &metadata)
    .await
{
    Ok(()) => println!("Success!"),
    Err(OptimisticCommandError::Concurrency(conflict)) => {
        println!(
            "Conflict: expected version {:?}, actual {:?}",
            conflict.expected,
            conflict.actual
        );
    }
    Err(e) => println!("Other error: {e}"),
}
```

## Handling Conflicts

The most common pattern for handling conflicts is to **retry** the operation.

The repository provides a helper for this: `execute_with_retry`.

```rust,ignore
use event_sourcing::RetryResult;

let attempts: RetryResult<MyAggregate, MyStore> =
    repo.execute_with_retry::<MyAggregate, MyCommand>(&id, &command, &metadata, 3).await?;
println!("Succeeded after {attempts} attempt(s)");
```

Each retry loads fresh state from the event store, so business rules are always validated
against the current aggregate state.

`max_retries` controls how many *retries after the first attempt* are allowed, so the
operation is attempted up to `max_retries + 1` times total.

## When to Use Optimistic Concurrency

**Use optimistic concurrency when:**

- Multiple writers might modify the same aggregate simultaneously
- Business rules depend on current state (e.g., balance checks, inventory limits)
- Data integrity is more important than write throughput

**Consider last-writer-wins when:**

- Aggregates are rarely modified concurrently
- Events are append-only without state-dependent validation
- You have a single writer per aggregate (e.g., actor-per-entity pattern)

## How It Works

1. When loading an aggregate, the repository records the current stream version
2. When committing, it passes the expected version to the event store
3. The store checks if the actual version matches the expected version
4. If they differ, the store returns a `ConcurrencyConflict` error

The `inmemory::Store` supports this via its `stream_version()` method and the `expected_version` parameter on `append()`.

## Example

See [`examples/optimistic_concurrency.rs`](https://github.com/danieleades/event-sourcing/blob/main/examples/optimistic_concurrency.rs)
for a complete working example demonstrating:

- Basic optimistic concurrency usage
- Conflict detection with concurrent modifications
- Retry patterns for handling conflicts
- Business rule enforcement with fresh state
