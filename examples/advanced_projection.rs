//! Advanced projection example using the builder API directly.
//!
//! This example demonstrates how to compose a projection that mixes
//! global and scoped event subscriptions without relying on the
//! any derive helper. We manually register:
//!
//! - All `ProductRestocked` events (global)
//! - `InventoryAdjusted` events scoped to a specific product SKU
//! - `SaleCompleted` events scoped to sale aggregates for the same SKU
//! - `PromotionApplied` events coming from a different aggregate kind

use std::collections::HashMap;

use event_sourcing::Aggregate;
use event_sourcing::{
    Apply, ApplyProjection, DomainEvent, EventStore, InMemoryEventStore, JsonCodec, Projection,
    Repository,
};
use serde::{Deserialize, Serialize};

// =============================================================================
// Aggregates and domain events
// =============================================================================

#[derive(Debug, Default, Aggregate)]
#[aggregate(id = String, error = String, events(ProductRestocked, InventoryAdjusted))]
struct Product;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProductRestocked {
    sku: String,
    quantity: i64,
}

impl DomainEvent for ProductRestocked {
    const KIND: &'static str = "inventory.product.restocked";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InventoryAdjusted {
    sku: String,
    delta: i64,
}

impl DomainEvent for InventoryAdjusted {
    const KIND: &'static str = "inventory.product.adjusted";
}

impl Apply<ProductRestocked> for Product {
    fn apply(&mut self, _event: &ProductRestocked) {}
}

impl Apply<InventoryAdjusted> for Product {
    fn apply(&mut self, _event: &InventoryAdjusted) {}
}

#[derive(Debug, Default, Aggregate)]
#[aggregate(id = String, error = String, events(SaleCompleted))]
struct Sale;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SaleCompleted {
    sale_id: String,
    product_sku: String,
    quantity: i64,
}

impl DomainEvent for SaleCompleted {
    const KIND: &'static str = "sales.sale.completed";
}

impl Apply<SaleCompleted> for Sale {
    fn apply(&mut self, _event: &SaleCompleted) {}
}

#[derive(Debug, Default, Aggregate)]
#[aggregate(id = String, error = String, events(PromotionApplied))]
struct Promotion;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PromotionApplied {
    promotion_id: String,
    product_sku: String,
    amount_cents: i64,
}

impl DomainEvent for PromotionApplied {
    const KIND: &'static str = "marketing.promotion.applied";
}

impl Apply<PromotionApplied> for Promotion {
    fn apply(&mut self, _event: &PromotionApplied) {}
}

// =============================================================================
// Manual projection
// =============================================================================

#[derive(Debug, Default)]
struct ProductSummary {
    stock_levels: HashMap<String, i64>,
    sales: HashMap<String, i64>,
    promotion_totals: HashMap<String, i64>,
}

impl Projection for ProductSummary {
    type Metadata = ();
}

impl ApplyProjection<ProductRestocked, ()> for ProductSummary {
    fn apply_projection(&mut self, _aggregate_id: &str, event: &ProductRestocked, _metadata: &()) {
        *self.stock_levels.entry(event.sku.clone()).or_default() += event.quantity;
    }
}

impl ApplyProjection<InventoryAdjusted, ()> for ProductSummary {
    fn apply_projection(&mut self, _aggregate_id: &str, event: &InventoryAdjusted, _metadata: &()) {
        *self.stock_levels.entry(event.sku.clone()).or_default() += event.delta;
    }
}

impl ApplyProjection<SaleCompleted, ()> for ProductSummary {
    fn apply_projection(&mut self, _aggregate_id: &str, event: &SaleCompleted, _metadata: &()) {
        *self.sales.entry(event.product_sku.clone()).or_default() += event.quantity;
    }
}

impl ApplyProjection<PromotionApplied, ()> for ProductSummary {
    fn apply_projection(&mut self, _aggregate_id: &str, event: &PromotionApplied, _metadata: &()) {
        *self
            .promotion_totals
            .entry(event.product_sku.clone())
            .or_default() += event.amount_cents;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);
    let mut repository = Repository::new(store);

    let product_id = String::from("SKU-007");
    let sale_id = String::from("sale-123");
    let promotion_id = String::from("promo-42");

    // Seed the store manually using the aggregate event enums.
    let mut product_tx = repository
        .event_store_mut()
        .begin(Product::KIND, &product_id);
    product_tx.append(
        ProductEvent::from(ProductRestocked {
            sku: "SKU-007".into(),
            quantity: 50,
        }),
        (),
    )?;
    product_tx.append(
        ProductEvent::from(InventoryAdjusted {
            sku: "SKU-007".into(),
            delta: -5,
        }),
        (),
    )?;
    product_tx.commit()?;

    let mut sale_tx = repository.event_store_mut().begin(Sale::KIND, &sale_id);
    sale_tx.append(
        SaleEvent::from(SaleCompleted {
            sale_id: "sale-123".into(),
            product_sku: "SKU-007".into(),
            quantity: 2,
        }),
        (),
    )?;
    sale_tx.commit()?;

    let mut promo_tx = repository
        .event_store_mut()
        .begin(Promotion::KIND, &promotion_id);
    promo_tx.append(
        PromotionEvent::from(PromotionApplied {
            promotion_id: "promo-42".into(),
            product_sku: "SKU-007".into(),
            amount_cents: 300,
        }),
        (),
    )?;
    promo_tx.commit()?;

    // Build the projection with mixed filters.
    let summary = repository
        .build_projection::<ProductSummary>()
        .event::<ProductRestocked>() // global restocks
        .event_for::<Product, InventoryAdjusted>(&product_id)
        .event_for::<Sale, SaleCompleted>(&sale_id)
        .event_for::<Promotion, PromotionApplied>(&promotion_id)
        .load()?;

    println!("Product summary: {summary:#?}");

    Ok(())
}
