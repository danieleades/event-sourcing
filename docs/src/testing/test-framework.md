# Test Framework

The crate provides two testing utilities:

- **`TestFramework`**: Unit testing aggregates in isolation (no stores, no serialization)
- **`RepositoryTestExt`**: Integration testing with real repositories (seeding data, simulating concurrency)

## Unit Testing with TestFramework

`TestFramework` tests aggregates in isolation using the Given-When-Then pattern. No stores, no serialization—just pure domain logic.

## Enabling the Test Framework

Add the `test-util` feature to your dev dependencies:

```toml
[dev-dependencies]
event-sourcing = { version = "0.1", features = ["test-util"] }
```

## Basic Usage

```rust,ignore
use event_sourcing::test::TestFramework;

type AccountTest = TestFramework<Account>;

#[test]
fn deposit_increases_balance() {
    AccountTest::new()
        .given(vec![FundsDeposited { amount: 100 }.into()])
        .when(&Deposit { amount: 50 })
        .then_expect_events(&[FundsDeposited { amount: 50 }.into()]);
}
```

## The Given-When-Then Pattern

```mermaid
flowchart LR
    Given["Given<br/>(past events)"] --> When["When<br/>(command)"]
    When --> Then["Then<br/>(expected outcome)"]
```

- **Given**: Events that have already occurred (establishes state)
- **When**: The command being tested
- **Then**: Expected events or error

## Given Methods

### `given(events)`

Start with a list of past events:

```rust,ignore
AccountTest::new()
    .given(vec![
        FundsDeposited { amount: 100 }.into(),
        FundsWithdrawn { amount: 30 }.into(),
    ])
    // Balance is now 70
```

### `given_no_previous_events()`

Start with a fresh aggregate:

```rust,ignore
AccountTest::new()
    .given_no_previous_events()
    // Balance is 0
```

## When Methods

### `when(command)`

Execute a command against the aggregate:

```rust,ignore
.when(&Withdraw { amount: 50 })
```

## Then Methods

### `then_expect_events(events)`

Assert that specific events were produced:

```rust,ignore
.then_expect_events(&[
    FundsWithdrawn { amount: 50 }.into(),
]);
```

Events are compared using `PartialEq`, so your event types must derive it.

### `then_expect_no_events()`

Assert that the command produced no events:

```rust,ignore
AccountTest::new()
    .given_no_previous_events()
    .when(&Deposit { amount: 0 })  // No-op deposit
    .then_expect_no_events();
```

### `then_expect_error()`

Assert that the command failed (any error):

```rust,ignore
AccountTest::new()
    .given(vec![FundsDeposited { amount: 50 }.into()])
    .when(&Withdraw { amount: 100 })  // Insufficient funds
    .then_expect_error();
```

### `then_expect_error_eq(error)`

Assert a specific error:

```rust,ignore
.then_expect_error_eq(&AccountError::InsufficientFunds);
```

### `then_expect_error_message(substring)`

Assert the error message contains a substring:

```rust,ignore
.then_expect_error_message("insufficient");
```

### `inspect_result(closure)`

Custom assertions on the result:

```rust,ignore
.inspect_result(|result| {
    let events = result.as_ref().unwrap();
    assert_eq!(events.len(), 2);
    // Custom validation...
});
```

## Complete Test Suite Example

```rust,ignore
use event_sourcing::test::TestFramework;

type AccountTest = TestFramework<Account>;

#[test]
fn deposits_positive_amount() {
    AccountTest::new()
        .given_no_previous_events()
        .when(&Deposit { amount: 100 })
        .then_expect_events(&[FundsDeposited { amount: 100 }.into()]);
}

#[test]
fn rejects_overdraft() {
    AccountTest::new()
        .given(vec![FundsDeposited { amount: 100 }.into()])
        .when(&Withdraw { amount: 150 })
        .then_expect_error_eq(&AccountError::InsufficientFunds);
}

#[test]
fn rejects_invalid_deposit() {
    AccountTest::new()
        .given_no_previous_events()
        .when(&Deposit { amount: -50 })
        .then_expect_error();
}
```

## Testing Projections

Projections don't use `TestFramework`. Test them directly:

```rust,ignore
#[test]
fn projection_aggregates_deposits() {
    let mut proj = AccountSummary::default();

    proj.apply_projection("ACC-001", &FundsDeposited { amount: 100 }, &());
    proj.apply_projection("ACC-002", &FundsDeposited { amount: 50 }, &());
    proj.apply_projection("ACC-001", &FundsDeposited { amount: 25 }, &());

    assert_eq!(proj.accounts.get("ACC-001"), Some(&125));
    assert_eq!(proj.accounts.get("ACC-002"), Some(&50));
}
```

## Next

[Design Decisions](../reference/design-decisions.md) — Why the crate works this way
