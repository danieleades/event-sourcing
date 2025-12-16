//! Inventory Report Example - Pure Events with Composite IDs
//!
//! Demonstrates the event envelope pattern with pure domain events and
//! composite aggregate IDs, using the `#[derive(Aggregate)]` macro and the
//! manual `ProjectionBuilder` API.
//! - **Product aggregate**: Manages inventory with simple string IDs (SKU)
//! - **Sale aggregate**: Records sales with composite IDs (`SaleId {
//!   product_sku, sale_number }`)
//! - **`InventoryReport` projection**: Correlates events across aggregates via
//!   envelope metadata
//!
//! Key architectural points:
//! - **Pure events**: Domain events contain no persistence metadata
//! - **Composite IDs**: Sale aggregate IDs encode the product SKU in
//!   `aggregate_id`
//! - **`ApplyProjection` + builder**: Projections access `aggregate_kind`,
//!   `aggregate_id`, and metadata
//! - **External IDs**: Aggregates treat IDs as infrastructure metadata supplied
//!   by the repository

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sourcery::{
    Aggregate, Apply, ApplyProjection, DomainEvent, Handle, Projection, Repository,
    store::{JsonCodec, inmemory},
};

// =============================================================================
// Product Aggregate - PURE EVENTS (no IDs embedded)
// =============================================================================

// Commands for the product aggregate
#[derive(Debug)]
pub struct Restock {
    pub quantity: i64,
    pub unit_price_cents: i64,
}

#[derive(Debug)]
pub struct AdjustInventory {
    pub quantity_delta: i64,
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(id = String, error = String, events(ProductRestocked, InventoryAdjusted))]
pub struct Product {
    // No SKU field! The ID is external metadata
    quantity: i64,
    unit_price_cents: i64,
}

// Pure domain events - no infrastructure data
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProductRestocked {
    pub quantity: i64,
    pub unit_price_cents: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryAdjusted {
    pub quantity_delta: i64,
    pub reason: String,
}

impl DomainEvent for ProductRestocked {
    const KIND: &'static str = "inventory.product.restocked";
}

impl DomainEvent for InventoryAdjusted {
    const KIND: &'static str = "inventory.product.adjusted";
}

impl Apply<ProductRestocked> for Product {
    fn apply(&mut self, event: &ProductRestocked) {
        self.quantity += event.quantity;
        self.unit_price_cents = event.unit_price_cents;
    }
}

impl Apply<InventoryAdjusted> for Product {
    fn apply(&mut self, event: &InventoryAdjusted) {
        self.quantity += event.quantity_delta;
    }
}

// Handle<C> implementations for each command
impl Handle<Restock> for Product {
    fn handle(&self, command: &Restock) -> Result<Vec<Self::Event>, Self::Error> {
        if command.quantity <= 0 {
            return Err("quantity must be positive".to_string());
        }
        Ok(vec![
            ProductRestocked {
                quantity: command.quantity,
                unit_price_cents: command.unit_price_cents,
            }
            .into(),
        ])
    }
}

impl Handle<AdjustInventory> for Product {
    fn handle(&self, command: &AdjustInventory) -> Result<Vec<Self::Event>, Self::Error> {
        let new_quantity = self.quantity + command.quantity_delta;
        if new_quantity < 0 {
            return Err(format!(
                "adjustment would result in negative inventory: {new_quantity}"
            ));
        }
        Ok(vec![
            InventoryAdjusted {
                quantity_delta: command.quantity_delta,
                reason: command.reason.clone(),
            }
            .into(),
        ])
    }
}

// =============================================================================
// Sale Aggregate - PURE EVENTS with Composite ID
// =============================================================================

/// Composite ID for Sale aggregate
///
/// Encodes both the product SKU and sale number. The product SKU embedded in
/// the ID allows projections to correlate sale events with product inventory
/// without polluting the event data with cross-aggregate references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaleId {
    pub product_sku: String,
    pub sale_number: String,
}

impl std::fmt::Display for SaleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", self.product_sku, self.sale_number)
    }
}

// Commands for the sale aggregate
#[derive(Debug)]
pub struct CompleteSale {
    pub quantity: i64,
    pub sale_price_cents: i64,
}

#[derive(Debug)]
pub struct RefundSale {
    pub amount_cents: i64,
}

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(id = String, error = String, events(SaleCompleted, SaleRefunded))]
pub struct Sale {
    total_cents: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaleCompleted {
    pub quantity: i64,
    pub sale_price_cents: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaleRefunded {
    pub amount_cents: i64,
}

impl DomainEvent for SaleCompleted {
    const KIND: &'static str = "sales.sale.completed";
}

impl DomainEvent for SaleRefunded {
    const KIND: &'static str = "sales.sale.refunded";
}

impl Apply<SaleCompleted> for Sale {
    fn apply(&mut self, event: &SaleCompleted) {
        self.total_cents += event.quantity * event.sale_price_cents;
    }
}

impl Apply<SaleRefunded> for Sale {
    fn apply(&mut self, event: &SaleRefunded) {
        self.total_cents -= event.amount_cents;
    }
}

impl Handle<CompleteSale> for Sale {
    fn handle(&self, command: &CompleteSale) -> Result<Vec<Self::Event>, Self::Error> {
        if command.quantity <= 0 {
            return Err("sale quantity must be positive".to_string());
        }
        Ok(vec![
            SaleCompleted {
                quantity: command.quantity,
                sale_price_cents: command.sale_price_cents,
            }
            .into(),
        ])
    }
}

impl Handle<RefundSale> for Sale {
    fn handle(&self, command: &RefundSale) -> Result<Vec<Self::Event>, Self::Error> {
        if command.amount_cents <= 0 {
            return Err("refund amount must be positive".to_string());
        }
        Ok(vec![
            SaleRefunded {
                amount_cents: command.amount_cents,
            }
            .into(),
        ])
    }
}

// =============================================================================
// Projection constructed manually through ProjectionBuilder
// =============================================================================

#[derive(Debug, Default)]
pub struct InventoryReport {
    pub total_products_restocked: i64,
    pub total_items_in_stock: i64,
    pub total_inventory_value_cents: i64,
    pub total_sales_completed: i64,
    pub total_sales_revenue_cents: i64,
    pub total_refunds_cents: i64,
    pub products_by_sku: HashMap<String, ProductStats>,
}

#[derive(Debug, Default, Clone)]
pub struct ProductStats {
    pub quantity: i64,
    pub unit_price_cents: i64,
    pub times_restocked: i64,
    pub units_sold: i64,
}

impl Projection for InventoryReport {
    type Id = String;
    type Metadata = ();
}

// ApplyProjection implementations - for events that need stream context
impl ApplyProjection<ProductRestocked> for InventoryReport {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &ProductRestocked,
        _metadata: &Self::Metadata,
    ) {
        // aggregate_id already has the "product::" prefix stripped
        let sku = aggregate_id;

        self.total_products_restocked += 1;
        self.total_items_in_stock += event.quantity;
        self.total_inventory_value_cents += event.quantity * event.unit_price_cents;

        let stats = self.products_by_sku.entry(sku.clone()).or_default();
        stats.quantity += event.quantity;
        stats.unit_price_cents = event.unit_price_cents;
        stats.times_restocked += 1;
    }
}

impl ApplyProjection<InventoryAdjusted> for InventoryReport {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &InventoryAdjusted,
        _metadata: &Self::Metadata,
    ) {
        // aggregate_id already has the "product::" prefix stripped
        let sku = aggregate_id;

        self.total_items_in_stock += event.quantity_delta;

        let stats = self.products_by_sku.entry(sku.clone()).or_default();
        let old_value = stats.quantity * stats.unit_price_cents;
        stats.quantity += event.quantity_delta;
        let new_value = stats.quantity * stats.unit_price_cents;
        self.total_inventory_value_cents += new_value - old_value;
    }
}

// ApplyProjection implementation - needs aggregate_id to extract product SKU
impl ApplyProjection<SaleCompleted> for InventoryReport {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &SaleCompleted,
        _metadata: &Self::Metadata,
    ) {
        // Extract product SKU from aggregate_id format: "{product_sku}::{sale_number}"
        // (the "sale::" kind prefix has already been stripped)
        let sku = aggregate_id.split("::").next().unwrap_or(aggregate_id);

        self.total_sales_completed += 1;
        let sale_amount = event.quantity * event.sale_price_cents;
        self.total_sales_revenue_cents += sale_amount;

        if let Some(stats) = self.products_by_sku.get_mut(sku) {
            let value_reduction = event.quantity * stats.unit_price_cents;
            self.total_items_in_stock -= event.quantity;
            self.total_inventory_value_cents -= value_reduction;
            stats.quantity -= event.quantity;
            stats.units_sold += event.quantity;
        }
    }
}

impl ApplyProjection<SaleRefunded> for InventoryReport {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &SaleRefunded,
        _metadata: &Self::Metadata,
    ) {
        self.total_refunds_cents += event.amount_cents;
        self.total_sales_revenue_cents -= event.amount_cents;
    }
}

// =============================================================================
// Example Usage
// =============================================================================

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = inmemory::Store::new(JsonCodec);
    let mut repository = Repository::new(store);

    println!("=== Inventory Management System ===\n");

    // Restock products
    println!("1. Restocking products...");
    repository
        .execute_command::<Product, Restock>(
            &"WIDGET-001".to_string(),
            &Restock {
                quantity: 100,
                unit_price_cents: 2500,
            },
            &(),
        )
        .await?;

    repository
        .execute_command::<Product, Restock>(
            &"GADGET-002".to_string(),
            &Restock {
                quantity: 50,
                unit_price_cents: 5000,
            },
            &(),
        )
        .await?;

    // Make sales - using composite SaleId that encodes product reference
    println!("2. Processing sales...");
    let sale1_id = SaleId {
        product_sku: "WIDGET-001".to_string(),
        sale_number: "001".to_string(),
    }
    .to_string();
    let sale2_id = SaleId {
        product_sku: "GADGET-002".to_string(),
        sale_number: "002".to_string(),
    }
    .to_string();

    repository
        .execute_command::<Sale, CompleteSale>(
            &sale1_id,
            &CompleteSale {
                quantity: 10,
                sale_price_cents: 2500,
            },
            &(),
        )
        .await?;

    repository
        .execute_command::<Sale, CompleteSale>(
            &sale2_id,
            &CompleteSale {
                quantity: 5,
                sale_price_cents: 5000,
            },
            &(),
        )
        .await?;

    // Adjust inventory
    println!("3. Adjusting inventory for damaged goods...");
    repository
        .execute_command::<Product, AdjustInventory>(
            &"WIDGET-001".to_string(),
            &AdjustInventory {
                quantity_delta: -3,
                reason: "damaged in warehouse".to_string(),
            },
            &(),
        )
        .await?;

    // Process refund
    println!("4. Processing refund...");
    repository
        .execute_command::<Sale, RefundSale>(&sale1_id, &RefundSale { amount_cents: 5000 }, &())
        .await?;

    // Additional restocking
    println!("5. Additional restocking...");
    repository
        .execute_command::<Product, Restock>(
            &"WIDGET-001".to_string(),
            &Restock {
                quantity: 25,
                unit_price_cents: 2500,
            },
            &(),
        )
        .await?;

    // Load global report
    println!("\n6. Loading global inventory report...\n");
    let report = repository
        .build_projection::<InventoryReport>()
        .event::<ProductRestocked>()
        .event::<InventoryAdjusted>()
        .event::<SaleCompleted>()
        .event::<SaleRefunded>()
        .load()
        .await?;

    // Display report
    println!("=== INVENTORY REPORT ===");
    println!(
        "Total Products Restocked: {}",
        report.total_products_restocked
    );
    println!("Total Items In Stock: {}", report.total_items_in_stock);
    println!(
        "Total Inventory Value: ${:.2}",
        report.total_inventory_value_cents as f64 / 100.0
    );
    println!("Total Sales Completed: {}", report.total_sales_completed);
    println!(
        "Total Sales Revenue: ${:.2}",
        report.total_sales_revenue_cents as f64 / 100.0
    );
    println!(
        "Total Refunds: ${:.2}",
        report.total_refunds_cents as f64 / 100.0
    );

    println!("\nProduct Stats by SKU:");
    for (sku, stats) in &report.products_by_sku {
        println!("  - {sku}:");
        println!("      Quantity: {}", stats.quantity);
        println!("      Unit price (cents): {}", stats.unit_price_cents);
        println!("      Times restocked: {}", stats.times_restocked);
        println!("      Units sold: {}", stats.units_sold);
    }

    Ok(())
}
