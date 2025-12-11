# Optimistic Concurrency

When multiple processes or threads write to the same aggregate simultaneously, you risk
losing updates. Optimistic concurrency control detects these conflicts by checking that
the stream version hasn't changed between loading the aggregate and committing new events.

## Enabling Optimistic Concurrency

By default, repositories use **last-writer-wins** semantics—no version checking is
performed. To enable optimistic concurrency, call `with_optimistic_concurrency()`:

```rust,ignore
use event_sourcing::{Repository, InMemoryEventStore, JsonCodec};

let store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);
let mut repo = Repository::new(store)
    .with_optimistic_concurrency();
```

This returns a `Repository<S, SS, Optimistic>` instead of `Repository<S, SS, Unchecked>`.
The concurrency strategy is encoded in the type system, so you get compile-time guarantees
about which error types you need to handle.

## Error Types

The two concurrency strategies use different error types:

| Strategy | Error Type | Includes Concurrency Variant? |
|----------|------------|-------------------------------|
| `Unchecked` (default) | `CommandError` | No |
| `Optimistic` | `OptimisticCommandError` | Yes |

When using optimistic concurrency, `execute_command` returns
`OptimisticCommandError::Concurrency(conflict)` if the stream version changed between
loading and committing:

```rust,ignore
use event_sourcing::OptimisticCommandError;

match repo.execute_command::<MyAggregate, MyCommand>(&id, &command, &metadata) {
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

The most common pattern for handling conflicts is to **retry** the operation:

```rust,ignore
fn execute_with_retry<A, C>(
    repo: &mut Repository<S, SS, Optimistic>,
    id: &A::Id,
    command: &C,
    metadata: &S::Metadata,
    max_retries: usize,
) -> Result<(), OptimisticCommandError<...>>
where
    A: Aggregate + Handle<C>,
{
    for _ in 0..max_retries {
        match repo.execute_command::<A, C>(id, command, metadata) {
            Ok(()) => return Ok(()),
            Err(OptimisticCommandError::Concurrency(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    // Final attempt
    repo.execute_command::<A, C>(id, command, metadata)
}
```

Each retry loads fresh state from the event store, so business rules are always validated
against the current aggregate state.

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

The `InMemoryEventStore` supports this via its `stream_version()` method and the
`expected_version` parameter on `append()`.

## Example

See [`examples/optimistic_concurrency.rs`](https://github.com/danieleades/event-sourcing/blob/main/examples/optimistic_concurrency.rs)
for a complete working example demonstrating:

- Basic optimistic concurrency usage
- Conflict detection with concurrent modifications
- Retry patterns for handling conflicts
- Business rule enforcement with fresh state
