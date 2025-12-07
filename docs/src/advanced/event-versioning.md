# Event Versioning

Events are immutable and stored forever. When your domain model evolves, you need strategies to read old events with new code.

## The Challenge

```d2
direction: right

2024: {
  V1: |md
    UserRegistered
    { email: String }
  |
}

2025: {
  V2: |md
    UserRegistered
    { email: String,
      marketing_consent: bool }
  |
}

2024.V1 -> 2025.V2: "How to read?"
```

Old events lack fields that new code expects.

## Strategy 1: Serde Defaults

The simplest approach—use serde attributes:

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRegistered {
    pub email: String,
    #[serde(default)]
    pub marketing_consent: bool,  // Defaults to false for old events
}
```

Works for:
- Adding optional fields
- Fields with sensible defaults

## Strategy 2: Explicit Versioning

Keep old event types, migrate at deserialization:

```rust,ignore
// Original event (still in storage)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRegisteredV1 {
    pub email: String,
}

// Current event
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRegistered {
    pub email: String,
    pub marketing_consent: bool,
}

impl From<UserRegisteredV1> for UserRegistered {
    fn from(v1: UserRegisteredV1) -> Self {
        Self {
            email: v1.email,
            marketing_consent: false, // Assumed for old users
        }
    }
}
```

## Strategy 3: Using serde-evolve

The [`serde-evolve`](https://crates.io/crates/serde-evolve) crate automates version chains:

```rust,ignore
use serde_evolve::Evolve;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRegisteredV1 {
    pub email: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRegisteredV2 {
    pub email: String,
    pub marketing_consent: bool,
}

impl From<UserRegisteredV1> for UserRegisteredV2 {
    fn from(v1: UserRegisteredV1) -> Self {
        Self { email: v1.email, marketing_consent: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Evolve)]
#[evolve(ancestors(UserRegisteredV1, UserRegisteredV2))]
pub struct UserRegistered {
    pub email: String,
    pub marketing_consent: bool,
    pub signup_source: Option<String>,  // New in V3
}

impl From<UserRegisteredV2> for UserRegistered {
    fn from(v2: UserRegisteredV2) -> Self {
        Self {
            email: v2.email,
            marketing_consent: v2.marketing_consent,
            signup_source: None,
        }
    }
}
```

When deserializing, `serde-evolve` tries each ancestor in order and applies the `From` chain.

## Strategy 4: Codec-Level Migration

Handle versioning in a custom codec:

```rust,ignore
pub struct VersionedJsonCodec;

impl Codec for VersionedJsonCodec {
    type Error = serde_json::Error;

    fn deserialize<E: DeserializeOwned>(&self, data: &[u8]) -> Result<E, Self::Error> {
        // Parse as Value first
        let mut value: serde_json::Value = serde_json::from_slice(data)?;

        // Apply migrations based on type or missing fields
        if value.get("marketing_consent").is_none() {
            value["marketing_consent"] = serde_json::Value::Bool(false);
        }

        // Deserialize to target type
        serde_json::from_value(value)
    }

    fn serialize<E: Serialize>(&self, event: &E) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(event)
    }
}
```

## Which Strategy to Use?

| Scenario | Recommended Approach |
|----------|---------------------|
| Adding optional field | Serde defaults |
| Adding required field with known default | Serde defaults |
| Complex migration logic | Explicit versions + From |
| Multiple version hops | serde-evolve |
| Schema changes with external validation | Codec-level |

## Event KIND Stability

The `KIND` constant must never change for stored events:

```rust,ignore
// BAD: Changing KIND breaks deserialization
impl DomainEvent for UserRegistered {
    const KIND: &'static str = "user.created";  // Was "user.registered"
}

// GOOD: Use new event type, migrate in code
impl DomainEvent for UserCreated {
    const KIND: &'static str = "user.created";
}
// Old UserRegistered events still deserialize, then convert
```

## Testing Migrations

Include serialized old events in your test suite:

```rust,ignore
#[test]
fn deserializes_v1_events() {
    let v1_json = r#"{"email":"old@example.com"}"#;
    let event: UserRegistered = serde_json::from_str(v1_json).unwrap();
    assert!(!event.marketing_consent);
}
```

This catches regressions when refactoring.

## Next

[Custom Metadata](custom-metadata.md) — Adding infrastructure context to events
