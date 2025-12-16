# Manual Implementation

While `#[derive(Aggregate)]` handles most cases, you might implement the traits manually for:

- Custom serialization logic
- Non-standard event routing
- Learning how the system works

## Side-by-Side Comparison

### With Derive Macro

```rust,ignore
use sourcery::{Aggregate, Apply, DomainEvent, Handle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsDeposited { pub amount: i64 }

impl DomainEvent for FundsDeposited {
    const KIND: &'static str = "account.deposited";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsWithdrawn { pub amount: i64 }

impl DomainEvent for FundsWithdrawn {
    const KIND: &'static str = "account.withdrawn";
}

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(id = String, error = String, events(FundsDeposited, FundsWithdrawn))]
pub struct Account {
    balance: i64,
}

impl Apply<FundsDeposited> for Account {
    fn apply(&mut self, e: &FundsDeposited) { self.balance += e.amount; }
}

impl Apply<FundsWithdrawn> for Account {
    fn apply(&mut self, e: &FundsWithdrawn) { self.balance -= e.amount; }
}
```

### Without Derive Macro

```rust,ignore
use sourcery::{Aggregate, DomainEvent};
use sourcery::codec::{Codec, EventDecodeError, ProjectionEvent, SerializableEvent};
use sourcery::store::PersistableEvent;
use serde::{Deserialize, Serialize};

// Events (same as before)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsDeposited { pub amount: i64 }

impl DomainEvent for FundsDeposited {
    const KIND: &'static str = "account.deposited";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsWithdrawn { pub amount: i64 }

impl DomainEvent for FundsWithdrawn {
    const KIND: &'static str = "account.withdrawn";
}

// Manual event enum
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AccountEvent {
    Deposited(FundsDeposited),
    Withdrawn(FundsWithdrawn),
}

// From implementations for ergonomic .into()
impl From<FundsDeposited> for AccountEvent {
    fn from(e: FundsDeposited) -> Self { Self::Deposited(e) }
}

impl From<FundsWithdrawn> for AccountEvent {
    fn from(e: FundsWithdrawn) -> Self { Self::Withdrawn(e) }
}

// SerializableEvent for persistence
impl SerializableEvent for AccountEvent {
    fn to_persistable<C: Codec, M>(
        self,
        codec: &C,
        metadata: M,
    ) -> Result<PersistableEvent<M>, C::Error> {
        let (kind, data) = match &self {
            Self::Deposited(e) => (FundsDeposited::KIND, codec.serialize(e)?),
            Self::Withdrawn(e) => (FundsWithdrawn::KIND, codec.serialize(e)?),
        };
        Ok(PersistableEvent {
            kind: kind.to_string(),
            data,
            metadata,
        })
    }
}

// ProjectionEvent for loading
impl ProjectionEvent for AccountEvent {
    const EVENT_KINDS: &'static [&'static str] = &[
        FundsDeposited::KIND,
        FundsWithdrawn::KIND,
    ];

    fn from_stored<C: Codec>(
        kind: &str,
        data: &[u8],
        codec: &C,
    ) -> Result<Self, EventDecodeError<C::Error>> {
        match kind {
            FundsDeposited::KIND => Ok(Self::Deposited(
                codec.deserialize(data).map_err(EventDecodeError::Codec)?,
            )),
            FundsWithdrawn::KIND => Ok(Self::Withdrawn(
                codec.deserialize(data).map_err(EventDecodeError::Codec)?,
            )),
            _ => Err(EventDecodeError::UnknownKind {
                kind: kind.to_string(),
                expected: Self::EVENT_KINDS,
            }),
        }
    }
}

// Aggregate struct
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Account {
    balance: i64,
}

// Manual Aggregate implementation — Apply<E> not needed, just match directly
impl Aggregate for Account {
    const KIND: &'static str = "account";
    type Event = AccountEvent;
    type Error = String;
    type Id = String;

    fn apply(&mut self, event: Self::Event) {
        match event {
            AccountEvent::Deposited(e) => self.balance += e.amount,
            AccountEvent::Withdrawn(e) => self.balance -= e.amount,
        }
    }
}
```

## What You Gain

Manual implementation lets you:

1. **Custom enum structure** — Different variant names, nested enums
2. **Custom serialization** — Compress, encrypt, or version events
3. **Fallback handling** — Gracefully handle unknown event types
4. **Conditional logic** — Skip certain events during replay

## What You Lose

- More code to maintain
- Easy to introduce bugs in the match arms
- Must keep `EVENT_KINDS` array in sync with `from_stored`

## When to Go Manual

| Scenario | Recommendation |
|----------|----------------|
| Standard CRUD aggregate | Use derive macro |
| Learning the crate | Start with derive, then explore manual |
| Custom event codec | Manual |
| Dynamic event types | Manual |
| Unusual enum structure | Manual |

## Next

[Snapshots](../advanced/snapshots.md) — Optimizing aggregate loading
