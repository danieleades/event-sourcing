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

use serde::{Deserialize, Serialize};
use sourcery::Aggregate;
use sourcery::store::{JsonCodec, inmemory};
use sourcery::test::RepositoryTestExt;
use sourcery::{Apply, ApplyProjection, DomainEvent, Projection, Repository};

// =============================================================================
// Aggregates and domain events
// =============================================================================

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
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

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
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

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
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
    type Id = String;
    type Metadata = ();
}

impl ApplyProjection<ProductRestocked> for ProductSummary {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &ProductRestocked,
        _metadata: &Self::Metadata,
    ) {
        *self.stock_levels.entry(event.sku.clone()).or_default() += event.quantity;
    }
}

impl ApplyProjection<InventoryAdjusted> for ProductSummary {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &InventoryAdjusted,
        _metadata: &Self::Metadata,
    ) {
        *self.stock_levels.entry(event.sku.clone()).or_default() += event.delta;
    }
}

impl ApplyProjection<SaleCompleted> for ProductSummary {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &SaleCompleted,
        _metadata: &Self::Metadata,
    ) {
        *self.sales.entry(event.product_sku.clone()).or_default() += event.quantity;
    }
}

impl ApplyProjection<PromotionApplied> for ProductSummary {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &PromotionApplied,
        _metadata: &Self::Metadata,
    ) {
        *self
            .promotion_totals
            .entry(event.product_sku.clone())
            .or_default() += event.amount_cents;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
    let mut repository = Repository::new(store);

    let product_id = String::from("SKU-007");
    let sale_id = String::from("sale-123");
    let promotion_id = String::from("promo-42");

    // Seed the store using the test utilities.
    repository
        .seed_events::<Product>(
            &product_id,
            vec![
                ProductRestocked {
                    sku: "SKU-007".into(),
                    quantity: 50,
                }
                .into(),
                InventoryAdjusted {
                    sku: "SKU-007".into(),
                    delta: -5,
                }
                .into(),
            ],
        )
        .await?;

    repository
        .seed_events::<Sale>(
            &sale_id,
            vec![
                SaleCompleted {
                    sale_id: "sale-123".into(),
                    product_sku: "SKU-007".into(),
                    quantity: 2,
                }
                .into(),
            ],
        )
        .await?;

    repository
        .seed_events::<Promotion>(
            &promotion_id,
            vec![
                PromotionApplied {
                    promotion_id: "promo-42".into(),
                    product_sku: "SKU-007".into(),
                    amount_cents: 300,
                }
                .into(),
            ],
        )
        .await?;

    // Build the projection with mixed filters.
    let summary = repository
        .build_projection::<ProductSummary>()
        .event::<ProductRestocked>() // global restocks
        .event_for::<Product, InventoryAdjusted>(&product_id)
        .event_for::<Sale, SaleCompleted>(&sale_id)
        .event_for::<Promotion, PromotionApplied>(&promotion_id)
        .load()
        .await?;

    println!("Product summary: {summary:#?}");

    Ok(())
}
