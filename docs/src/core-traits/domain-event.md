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

Many event-sourcing libraries define events directly as enum variants:

```rust,ignore
// Other libraries
enum OrderEvent {
    Placed { product_id: String, quantity: u32 },
    Shipped { tracking_number: String },
}
```

This crate keeps events as separate structs:

```rust,ignore
// This crate
struct OrderPlaced { product_id: String, quantity: u32 }
struct OrderShipped { tracking_number: String }
```

Benefits:

| Approach | Struct-based (this crate) | Enum-based |
|----------|--------------------------|------------|
| **Reuse across aggregates** | Same event type in multiple aggregates | Must duplicate or share enum |
| **Cross-context sharing** | Import the struct | Import entire enum |
| **Projection decoupling** | Subscribe to specific event types | Must know about aggregate's enum |
| **Compile-time overhead** | Smaller per-aggregate | All variants compiled together |

The derive macro generates the enum internally—you get the best of both worlds.

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

1. **Include all necessary data** — Events should be self-describing

   ```rust,ignore
   // Good: includes price at time of order
   struct OrderPlaced {
       product_id: String,
       quantity: u32,
       unit_price: i64,  // Captured at order time
   }

   // Bad: requires lookup to know price
   struct OrderPlaced {
       product_id: String,
       quantity: u32,
   }
   ```

2. **Avoid IDs when possible** — Aggregate IDs travel in the envelope

   ```rust,ignore
   // Good: ID is in envelope
   struct FundsDeposited { amount: i64 }

   // Avoid: redundant ID in payload
   struct FundsDeposited { account_id: String, amount: i64 }
   ```

3. **Keep events small** — One fact per event

   ```rust,ignore
   // Good: separate events
   struct AddressChanged { new_address: Address }
   struct PhoneChanged { new_phone: String }

   // Avoid: combined event
   struct ContactInfoChanged {
       new_address: Option<Address>,
       new_phone: Option<String>,
   }
   ```

## Next

[Commands](commands.md) — Handling requests that produce events
