# The Aggregate Derive

The `#[derive(Aggregate)]` macro eliminates boilerplate by generating the event enum and trait implementations.

## Basic Usage

```rust,ignore
use sourcery::Aggregate;

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(id = String, error = String, events(FundsDeposited, FundsWithdrawn))]
pub struct Account {
    balance: i64,
}
```

## Attribute Reference

| Attribute | Required | Description |
|-----------|----------|-------------|
| `id = Type` | Yes | Aggregate identifier type |
| `error = Type` | Yes | Error type for command handling |
| `events(E1, E2, ...)` | Yes | Event types this aggregate produces |
| `kind = "name"` | No | Aggregate type identifier (default: lowercase struct name) |
| `event_enum = "Name"` | No | Generated enum name (default: `{Struct}Event`) |

## What Gets Generated

For this input:

```rust,ignore
#[derive(Aggregate)]
#[aggregate(id = String, error = AccountError, events(FundsDeposited, FundsWithdrawn))]
pub struct Account { balance: i64 }
```

The macro generates:

```d2
Input: |md
  #[derive(Aggregate)]
  struct Account
|

Input -> "enum AccountEvent"
Input -> "impl Aggregate for Account"
Input -> "impl From<FundsDeposited>"
Input -> "impl From<FundsWithdrawn>"
Input -> "impl SerializableEvent"
Input -> "impl ProjectionEvent"
```

### 1. Event Enum

```rust,ignore
pub enum AccountEvent {
    FundsDeposited(FundsDeposited),
    FundsWithdrawn(FundsWithdrawn),
}
```

### 2. From Implementations

```rust,ignore
impl From<FundsDeposited> for AccountEvent {
    fn from(event: FundsDeposited) -> Self {
        AccountEvent::FundsDeposited(event)
    }
}

impl From<FundsWithdrawn> for AccountEvent {
    fn from(event: FundsWithdrawn) -> Self {
        AccountEvent::FundsWithdrawn(event)
    }
}
```

This enables the `.into()` call in command handlers:

```rust,ignore
Ok(vec![FundsDeposited { amount: 100 }.into()])
```

### 3. Aggregate Implementation

```rust,ignore
impl Aggregate for Account {
    const KIND: &'static str = "account";
    type Event = AccountEvent;
    type Error = AccountError;
    type Id = String;

    fn apply(&mut self, event: Self::Event) {
        match event {
            AccountEvent::FundsDeposited(e) => Apply::apply(self, &e),
            AccountEvent::FundsWithdrawn(e) => Apply::apply(self, &e),
        }
    }
}
```

### 4. SerializableEvent Implementation

Converts domain events to persistable form:

```rust,ignore
impl SerializableEvent for AccountEvent {
    fn to_persistable<C: Codec, M>(
        self,
        codec: &C,
        metadata: M,
    ) -> Result<PersistableEvent<M>, C::Error> {
        match self {
            AccountEvent::FundsDeposited(e) => /* serialize with kind */,
            AccountEvent::FundsWithdrawn(e) => /* serialize with kind */,
        }
    }
}
```

### 5. ProjectionEvent Implementation

Deserializes stored events:

```rust,ignore
impl ProjectionEvent for AccountEvent {
    const EVENT_KINDS: &'static [&'static str] = &[
        "account.deposited",
        "account.withdrawn",
    ];

    fn from_stored<C: Codec>(
        kind: &str,
        data: &[u8],
        codec: &C,
    ) -> Result<Self, C::Error> {
        match kind {
            "account.deposited" => Ok(Self::FundsDeposited(codec.deserialize(data)?)),
            "account.withdrawn" => Ok(Self::FundsWithdrawn(codec.deserialize(data)?)),
            _ => /* unknown event error */,
        }
    }
}
```

## Customizing the Kind

By default, `KIND` is the lowercase struct name. Override it:

```rust,ignore
#[derive(Aggregate)]
#[aggregate(
    kind = "bank-account",
    id = String,
    error = String,
    events(FundsDeposited)
)]
pub struct Account { /* ... */ }
```

Now `Account::KIND` is `"bank-account"`.

## Customizing the Event Enum Name

```rust,ignore
#[derive(Aggregate)]
#[aggregate(
    event_enum = "BankEvent",
    id = String,
    error = String,
    events(FundsDeposited)
)]
pub struct Account { /* ... */ }
```

Now the generated enum is `BankEvent` instead of `AccountEvent`.

## Requirements

Your struct must also derive/implement:

- `Default` — Fresh aggregate state
- `Serialize` + `Deserialize` — For snapshotting

Each event type must implement `DomainEvent`.

## Next

[Manual Implementation](manual-implementation.md) — Implementing without the macro
