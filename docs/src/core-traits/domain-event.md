# Domain Events

Events are the heart of event sourcing. They represent facts—things that have happened. In this crate, events are first-class structs, not just enum variants.

## The `DomainEvent` Trait

```rust,ignore
pub trait DomainEvent: Serialize + DeserializeOwned {
    /// Unique identifier for this event type (used for serialization routing)
    const KIND: &'static str;
}
```

Every event struct implements this trait:

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderPlaced {
    pub product_id: String,
    pub quantity: u32,
    pub unit_price: i64,
}

impl DomainEvent for OrderPlaced {
    const KIND: &'static str = "order.placed";
}
```

## Why First-Class Structs?

This crate keeps events as separate structs rather than enum variants. Benefits:

- **Reuse** — Same event type in multiple aggregates
- **Decoupling** — Projections subscribe to event types, not aggregate enums
- **Smaller scope** — Import individual event structs

The derive macro generates the aggregate's event enum internally—you get the best of both worlds.

## Naming Conventions

Events are **past tense** because they describe things that already happened:

| Good | Bad |
|------|-----|
| `OrderPlaced` | `PlaceOrder` |
| `FundsDeposited` | `DepositFunds` |
| `UserRegistered` | `RegisterUser` |
| `PasswordChanged` | `ChangePassword` |

Use `KIND` constants that are stable and namespaced:

```rust,ignore
const KIND: &'static str = "order.placed";      // Good
const KIND: &'static str = "OrderPlaced";       // OK
const KIND: &'static str = "placed";            // Too generic
```

The `KIND` is stored in the event log and used for deserialization, so it must never change for existing events.

## Event Design Guidelines

1. **Include all necessary data** — Capture values at event time (e.g., price when ordered)
2. **Avoid aggregate IDs** — They travel in the envelope, not the event payload
3. **Keep events small** — One fact per event (prefer `AddressChanged` + `PhoneChanged` over `ContactInfoChanged`)

## Next

[Commands](commands.md) — Handling requests that produce events
