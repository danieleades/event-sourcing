//! Versioned Events Example
//!
//! Demonstrates using the `versioned` crate with event sourcing to handle
//! evolving event schemas and metadata over time.
//!
//! This example shows:
//! - **Versioned domain events**: Events that evolve through multiple versions
//! - **Versioned metadata**: Infrastructure metadata that changes over time
//! - **Transparent migration**: Old events automatically migrate to current schema
//! - **Event replay**: Historical events from different versions replaying correctly
//!
//! Real-world scenario: An order management system where:
//! - V1: Basic orders (product + quantity)
//! - V2: Added customer tracking (`customer_id`)
//! - V3: Added shipping (`delivery_address`)
//!
//! Run with: `cargo run --example versioned_events`

use event_sourcing::codec::ProjectionEvent;
use event_sourcing::store::EventStore;
use event_sourcing::store::{EventFilter, JsonCodec, inmemory};
use event_sourcing::{Aggregate, Apply, DomainEvent, Handle, Repository};
use serde::{Deserialize, Serialize};
use serde_evolve::Versioned;

// =============================================================================
// Versioned Domain Event - OrderPlaced
// =============================================================================

/// Current domain event for order placement.
///
/// This is what the application code works with. The versioned crate handles
/// deserializing historical versions and migrating them to this structure.
#[derive(Clone, Debug, Versioned)]
#[versioned(
    mode = "infallible",
    chain(order_placed_versions::V1, order_placed_versions::V2, order_placed_versions::V3),
    transparent = true  // Enable direct serde support
)]
pub struct OrderPlaced {
    pub product_sku: String,
    pub quantity: u32,
    pub customer_id: String,
    pub delivery_address: String,
}

/// Historical versions of the [`OrderPlaced`] event.
///
/// This module contains the wire format DTOs for each version. The versioned
/// crate automatically migrates from V1 → V2 → V3 → `OrderPlaced`.
mod order_placed_versions {
    use super::OrderPlaced;
    use serde::{Deserialize, Serialize};

    /// V1: Original event schema - just product and quantity
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct V1 {
        pub product_sku: String,
        pub quantity: u32,
    }

    /// V2: Added customer tracking
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct V2 {
        pub product_sku: String,
        pub quantity: u32,
        pub customer_id: String,
    }

    /// V3: Added shipping address
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct V3 {
        pub product_sku: String,
        pub quantity: u32,
        pub customer_id: String,
        pub delivery_address: String,
    }

    // Migration: V1 → V2
    impl From<V1> for V2 {
        fn from(v1: V1) -> Self {
            Self {
                product_sku: v1.product_sku,
                quantity: v1.quantity,
                customer_id: "UNKNOWN".to_string(), // Default for legacy events
            }
        }
    }

    // Migration: V2 → V3
    impl From<V2> for V3 {
        fn from(v2: V2) -> Self {
            Self {
                product_sku: v2.product_sku,
                quantity: v2.quantity,
                customer_id: v2.customer_id,
                delivery_address: "UNKNOWN".to_string(), // Default for legacy events
            }
        }
    }

    // Migration: V3 → Domain
    impl From<V3> for OrderPlaced {
        fn from(v3: V3) -> Self {
            Self {
                product_sku: v3.product_sku,
                quantity: v3.quantity,
                customer_id: v3.customer_id,
                delivery_address: v3.delivery_address,
            }
        }
    }

    // Serialization: Domain → V3 (always use latest version)
    impl From<&OrderPlaced> for V3 {
        fn from(event: &OrderPlaced) -> Self {
            Self {
                product_sku: event.product_sku.clone(),
                quantity: event.quantity,
                customer_id: event.customer_id.clone(),
                delivery_address: event.delivery_address.clone(),
            }
        }
    }
}

// Implement DomainEvent for the current version
impl DomainEvent for OrderPlaced {
    const KIND: &'static str = "order.placed";
}

// =============================================================================
// Versioned Metadata
// =============================================================================

/// Current metadata schema.
///
/// Like events, metadata can evolve over time. This shows how to version
/// infrastructure metadata alongside domain events.
#[derive(Clone, Debug, Default, Versioned)]
#[versioned(
    mode = "infallible",
    chain(metadata_versions::V1, metadata_versions::V2),
    transparent = true
)]
pub struct OrderMetadata {
    pub timestamp: Option<std::time::SystemTime>,
    pub user_id: Option<String>,
    pub request_id: Option<String>,
    pub client_version: Option<String>,
}

mod metadata_versions {
    use super::OrderMetadata;
    use serde::{Deserialize, Serialize};

    /// V1: Basic metadata
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct V1 {
        pub timestamp: Option<std::time::SystemTime>,
        pub user_id: Option<String>,
    }

    /// V2: Added request tracking
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct V2 {
        pub timestamp: Option<std::time::SystemTime>,
        pub user_id: Option<String>,
        pub request_id: Option<String>,
        pub client_version: Option<String>,
    }

    // Migration: V1 → V2
    impl From<V1> for V2 {
        fn from(v1: V1) -> Self {
            Self {
                timestamp: v1.timestamp,
                user_id: v1.user_id,
                request_id: None,
                client_version: None,
            }
        }
    }

    // Migration: V2 → Domain
    impl From<V2> for OrderMetadata {
        fn from(v2: V2) -> Self {
            Self {
                timestamp: v2.timestamp,
                user_id: v2.user_id,
                request_id: v2.request_id,
                client_version: v2.client_version,
            }
        }
    }

    // Serialization: Domain → V2
    impl From<&OrderMetadata> for V2 {
        fn from(metadata: &OrderMetadata) -> Self {
            Self {
                timestamp: metadata.timestamp,
                user_id: metadata.user_id.clone(),
                request_id: metadata.request_id.clone(),
                client_version: metadata.client_version.clone(),
            }
        }
    }
}

// =============================================================================
// Other Domain Events (unversioned for simplicity)
// =============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderShipped {
    pub tracking_number: String,
}

impl DomainEvent for OrderShipped {
    const KIND: &'static str = "order.shipped";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderCancelled {
    pub reason: String,
}

impl DomainEvent for OrderCancelled {
    const KIND: &'static str = "order.cancelled";
}

// =============================================================================
// Order Aggregate
// =============================================================================

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(id = String, error = String, events(OrderPlaced, OrderShipped, OrderCancelled))]
pub struct Order {
    items: Vec<OrderItem>,
    customer_id: Option<String>,
    delivery_address: Option<String>,
    status: OrderStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderItem {
    product_sku: String,
    quantity: u32,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
enum OrderStatus {
    #[default]
    Pending,
    Shipped,
    Cancelled,
}

impl Apply<OrderPlaced> for Order {
    fn apply(&mut self, event: &OrderPlaced) {
        self.items.push(OrderItem {
            product_sku: event.product_sku.clone(),
            quantity: event.quantity,
        });
        self.customer_id = Some(event.customer_id.clone());
        self.delivery_address = Some(event.delivery_address.clone());
    }
}

impl Apply<OrderShipped> for Order {
    fn apply(&mut self, _event: &OrderShipped) {
        self.status = OrderStatus::Shipped;
    }
}

impl Apply<OrderCancelled> for Order {
    fn apply(&mut self, _event: &OrderCancelled) {
        self.status = OrderStatus::Cancelled;
    }
}

// Command structs for Order aggregate
#[derive(Debug)]
pub struct PlaceOrder {
    pub product_sku: String,
    pub quantity: u32,
    pub customer_id: String,
    pub delivery_address: String,
}

#[derive(Debug)]
pub struct ShipOrder {
    pub tracking_number: String,
}

#[derive(Debug)]
pub struct CancelOrder {
    pub reason: String,
}

// Handle<C> implementations for each command
impl Handle<PlaceOrder> for Order {
    fn handle(&self, command: &PlaceOrder) -> Result<Vec<Self::Event>, Self::Error> {
        if !self.items.is_empty() {
            return Err("Order already exists".to_string());
        }
        Ok(vec![
            OrderPlaced {
                product_sku: command.product_sku.clone(),
                quantity: command.quantity,
                customer_id: command.customer_id.clone(),
                delivery_address: command.delivery_address.clone(),
            }
            .into(),
        ])
    }
}

impl Handle<ShipOrder> for Order {
    fn handle(&self, command: &ShipOrder) -> Result<Vec<Self::Event>, Self::Error> {
        if self.status != OrderStatus::Pending {
            return Err("Order cannot be shipped".to_string());
        }
        Ok(vec![
            OrderShipped {
                tracking_number: command.tracking_number.clone(),
            }
            .into(),
        ])
    }
}

impl Handle<CancelOrder> for Order {
    fn handle(&self, command: &CancelOrder) -> Result<Vec<Self::Event>, Self::Error> {
        if self.status != OrderStatus::Pending {
            return Err("Order cannot be cancelled".to_string());
        }
        Ok(vec![
            OrderCancelled {
                reason: command.reason.clone(),
            }
            .into(),
        ])
    }
}

// =============================================================================
// Example Usage
// =============================================================================

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Versioned Events with Event Sourcing ===\n");

    // Use OrderMetadata (versioned) as the metadata type for the store
    let store: inmemory::Store<String, JsonCodec, OrderMetadata> = inmemory::Store::new(JsonCodec);
    let mut repository = Repository::new(store);

    // ========================================
    // Simulate historical V1 event (old schema)
    // ========================================
    println!("1. Simulating historical V1 event from the past...");

    let v1_json = r#"{"_version":"1","product_sku":"WIDGET-001","quantity":5}"#;
    println!("   V1 JSON: {v1_json}\n");

    // With transparent mode, versioned events deserialize directly
    let v1_event: OrderPlaced = serde_json::from_str(v1_json)?;
    println!("   Migrated to current schema:");
    println!("     Product: {}", v1_event.product_sku);
    println!("     Quantity: {}", v1_event.quantity);
    println!("     Customer: {} (defaulted)", v1_event.customer_id);
    println!("     Address: {} (defaulted)\n", v1_event.delivery_address);

    // ========================================
    // Simulate historical V2 event
    // ========================================
    println!("2. Simulating historical V2 event...");

    let v2_json =
        r#"{"_version":"2","product_sku":"GADGET-002","quantity":3,"customer_id":"CUST-123"}"#;
    println!("   V2 JSON: {v2_json}\n");

    let v2_event: OrderPlaced = serde_json::from_str(v2_json)?;
    println!("   Migrated to current schema:");
    println!("     Product: {}", v2_event.product_sku);
    println!("     Quantity: {}", v2_event.quantity);
    println!("     Customer: {}", v2_event.customer_id);
    println!("     Address: {} (defaulted)\n", v2_event.delivery_address);

    // ========================================
    // Create new order with current schema (V3)
    // ========================================
    println!("3. Creating new order with current schema (V3)...");

    let order_id = "ORD-001".to_string();
    let metadata = OrderMetadata {
        timestamp: Some(std::time::SystemTime::now()),
        user_id: Some("user-789".to_string()),
        request_id: Some("req-abc123".to_string()),
        client_version: Some("v1.0.0".to_string()),
    };

    repository
        .execute_command::<Order, PlaceOrder>(
            &order_id,
            &PlaceOrder {
                product_sku: "DOOHICKEY-003".to_string(),
                quantity: 10,
                customer_id: "CUST-456".to_string(),
                delivery_address: "123 Main St, Anytown, USA".to_string(),
            },
            &metadata,
        )
        .await?;

    println!("   Order created: {order_id}");

    // Inspect the stored event
    let events = repository
        .event_store()
        .load_events(&[EventFilter::for_aggregate(
            OrderPlaced::KIND,
            Order::KIND,
            order_id.clone(),
        )])
        .await?;
    if let Some(stored_event) = events.first() {
        let json = String::from_utf8_lossy(&stored_event.data);
        println!("   Stored JSON: {json}\n");

        // Verify it's stored as V3 (current version)
        let parsed: serde_json::Value = serde_json::from_slice(&stored_event.data)?;
        assert_eq!(parsed["_version"], "3");
        println!("   ✓ Event stored as V3 (current version)");
    }

    // ========================================
    // Ship the order
    // ========================================
    println!("\n4. Shipping the order...");

    repository
        .execute_command::<Order, ShipOrder>(
            &order_id,
            &ShipOrder {
                tracking_number: "TRACK-123456".to_string(),
            },
            &metadata,
        )
        .await?;

    println!("   Order shipped with tracking: TRACK-123456");

    // ========================================
    // Replay all events to rebuild aggregate
    // ========================================
    println!("\n5. Replaying all events to rebuild aggregate state...");

    // Load all event kinds for this aggregate
    let events = repository
        .event_store()
        .load_events(&[
            EventFilter::for_aggregate(OrderPlaced::KIND, Order::KIND, order_id.clone()),
            EventFilter::for_aggregate(OrderShipped::KIND, Order::KIND, order_id.clone()),
            EventFilter::for_aggregate(OrderCancelled::KIND, Order::KIND, order_id.clone()),
        ])
        .await?;
    let codec = repository.event_store().codec();
    let mut order = Order::default();

    for stored_event in &events {
        let event = OrderEvent::from_stored(&stored_event.kind, &stored_event.data, codec)?;
        Aggregate::apply(&mut order, &event);
        println!("   Applied event: {}", stored_event.kind);
    }

    println!("\n   Final aggregate state:");
    println!("     Items: {} item(s)", order.items.len());
    if let Some(sku) = order.items.first() {
        println!("       - {}: {} units", sku.product_sku, sku.quantity);
    }
    println!("     Customer: {:?}", order.customer_id);
    println!("     Address: {:?}", order.delivery_address);
    println!("     Status: {:?}", order.status);

    // ========================================
    // Demonstrate versioned metadata
    // ========================================
    println!("\n6. Versioned metadata example...");

    let v1_metadata_json = r#"{"_version":"1","timestamp":null,"user_id":"user-123"}"#;
    let metadata_v1: OrderMetadata = serde_json::from_str(v1_metadata_json)?;

    println!("   V1 metadata migrated:");
    println!("     User: {:?}", metadata_v1.user_id);
    println!("     Request ID: {:?} (defaulted)", metadata_v1.request_id);
    println!(
        "     Client version: {:?} (defaulted)",
        metadata_v1.client_version
    );

    // ========================================
    // Summary
    // ========================================
    println!("\n=== Summary ===\n");
    println!("✓ V1 events (legacy) automatically migrate to current schema");
    println!("✓ V2 events (intermediate) automatically migrate to current schema");
    println!("✓ V3 events (current) used for new events");
    println!("✓ Metadata can also be versioned independently");
    println!("✓ Event replay works transparently across all versions");
    println!("✓ Application code only works with current domain model\n");

    println!("=== Example completed successfully! ===");
    Ok(())
}
