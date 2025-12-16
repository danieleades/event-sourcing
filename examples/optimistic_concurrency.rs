//! Demonstrates optimistic concurrency control for handling concurrent writes.
//!
//! Optimistic concurrency is the default for `Repository`. This example shows
//! how conflicts are detected and handled when multiple writers try to modify
//! the same aggregate simultaneously.
//!
//! Run with: `cargo run --example optimistic_concurrency`

use serde::{Deserialize, Serialize};
use sourcery::{
    Apply, DomainEvent, Handle, Repository,
    repository::OptimisticCommandError,
    store::{JsonCodec, inmemory},
    test::RepositoryTestExt,
};

// =============================================================================
// Domain Events
// =============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemReserved {
    pub quantity: u32,
}

impl DomainEvent for ItemReserved {
    const KIND: &'static str = "inventory.item.reserved";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemRestocked {
    pub quantity: u32,
}

impl DomainEvent for ItemRestocked {
    const KIND: &'static str = "inventory.item.restocked";
}

// =============================================================================
// Commands
// =============================================================================

#[derive(Debug)]
pub struct ReserveItem {
    pub quantity: u32,
}

#[derive(Debug)]
pub struct RestockItem {
    pub quantity: u32,
}

// =============================================================================
// Aggregate
// =============================================================================

#[derive(Debug, Default, Serialize, Deserialize, sourcery::Aggregate)]
#[aggregate(id = String, error = InventoryError, events(ItemReserved, ItemRestocked))]
pub struct InventoryItem {
    available: u32,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum InventoryError {
    #[error("insufficient stock: requested {requested}, available {available}")]
    InsufficientStock { requested: u32, available: u32 },
}

impl Apply<ItemReserved> for InventoryItem {
    fn apply(&mut self, event: &ItemReserved) {
        self.available = self.available.saturating_sub(event.quantity);
    }
}

impl Apply<ItemRestocked> for InventoryItem {
    fn apply(&mut self, event: &ItemRestocked) {
        self.available += event.quantity;
    }
}

impl Handle<ReserveItem> for InventoryItem {
    fn handle(&self, cmd: &ReserveItem) -> Result<Vec<Self::Event>, Self::Error> {
        if cmd.quantity > self.available {
            return Err(InventoryError::InsufficientStock {
                requested: cmd.quantity,
                available: self.available,
            });
        }
        Ok(vec![
            ItemReserved {
                quantity: cmd.quantity,
            }
            .into(),
        ])
    }
}

impl Handle<RestockItem> for InventoryItem {
    fn handle(&self, cmd: &RestockItem) -> Result<Vec<Self::Event>, Self::Error> {
        Ok(vec![
            ItemRestocked {
                quantity: cmd.quantity,
            }
            .into(),
        ])
    }
}

// =============================================================================
// Example Parts
// =============================================================================

type OptimisticRepo = Repository<inmemory::Store<String, JsonCodec, ()>>;

/// Part 1: Basic optimistic concurrency usage.
///
/// Demonstrates initializing inventory and making reservations without
/// conflicts.
async fn part1_basic_usage() -> Result<(OptimisticRepo, String), Box<dyn std::error::Error>> {
    println!("PART 1: Basic optimistic concurrency usage\n");

    let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store); // Optimistic concurrency is the default

    let item_id = "SKU-001".to_string();

    // Initialize inventory
    println!("1. Restocking item with 100 units...");
    repo.execute_command::<InventoryItem, RestockItem>(
        &item_id,
        &RestockItem { quantity: 100 },
        &(),
    )
    .await?;

    let item: InventoryItem = repo.aggregate_builder().load(&item_id).await?;
    println!("   Available: {}\n", item.available);

    // Normal reservation (no conflict)
    println!("2. Reserving 30 units (no concurrent modification)...");
    repo.execute_command::<InventoryItem, ReserveItem>(
        &item_id,
        &ReserveItem { quantity: 30 },
        &(),
    )
    .await?;

    let item: InventoryItem = repo.aggregate_builder().load(&item_id).await?;
    println!("   Available: {}\n", item.available);

    Ok((repo, item_id))
}

/// Part 2: Conflict detection.
///
/// Demonstrates how concurrent modifications are detected when loading fresh
/// state.
async fn part2_conflict_detection(
    repo: &mut OptimisticRepo,
    item_id: &String,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("PART 2: Demonstrating conflict detection\n");

    // Simulate a concurrent modification by injecting an event
    // This is what would happen if another process/thread modified the aggregate
    println!("3. Simulating concurrent modification (another process reserves 20 units)...");
    repo.inject_concurrent_event::<InventoryItem>(item_id, ItemReserved { quantity: 20 }.into())
        .await?;

    let item: InventoryItem = repo.aggregate_builder().load(item_id).await?;
    println!(
        "   Available after concurrent modification: {}\n",
        item.available
    );

    // Now try to reserve - this will succeed because execute_command loads fresh
    // state The conflict would only occur if we had pre-loaded state before the
    // concurrent modification
    println!("4. Reserving 10 more units (loads fresh state, so no conflict)...");
    repo.execute_command::<InventoryItem, ReserveItem>(item_id, &ReserveItem { quantity: 10 }, &())
        .await?;

    let item: InventoryItem = repo.aggregate_builder().load(item_id).await?;
    println!("   Available: {}\n", item.available);

    Ok(())
}

/// Part 3: Retry pattern for handling conflicts.
///
/// Demonstrates the built-in `execute_with_retry` method for automatic conflict
/// handling.
async fn part3_retry_pattern() -> Result<(OptimisticRepo, String), Box<dyn std::error::Error>> {
    println!("PART 3: Retry pattern for handling conflicts\n");

    // Create a fresh store to demonstrate retry more clearly
    let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store); // Optimistic concurrency is the default
    let item_id = "SKU-002".to_string();

    // Initialize
    repo.execute_command::<InventoryItem, RestockItem>(
        &item_id,
        &RestockItem { quantity: 50 },
        &(),
    )
    .await?;
    println!("5. Initialized SKU-002 with 50 units");

    // Inject a conflict before the retry helper runs
    // In a real system, this might be another service instance
    repo.inject_concurrent_event::<InventoryItem>(&item_id, ItemReserved { quantity: 5 }.into())
        .await?;
    println!("   Injected concurrent reservation of 5 units (simulating race condition)");

    // Use the built-in execute_with_retry method.
    // This automatically reloads and retries on ConcurrencyConflict errors.
    println!("\n6. Attempting to reserve 10 units with execute_with_retry...");
    let attempts = repo
        .execute_with_retry::<InventoryItem, _>(&item_id, &ReserveItem { quantity: 10 }, &(), 3)
        .await?;
    println!("   Succeeded on attempt {attempts}");

    let item: InventoryItem = repo.aggregate_builder().load(&item_id).await?;
    println!(
        "   Final available: {} (50 - 5 - 10 = 35)\n",
        item.available
    );

    Ok((repo, item_id))
}

/// Part 4: Business rule enforcement.
///
/// Demonstrates that business rules are enforced against fresh state.
async fn part4_business_rules(repo: &mut OptimisticRepo, item_id: &String) {
    println!("PART 4: Business rules with optimistic concurrency\n");

    println!("7. Attempting to reserve 40 units (should fail - only 35 available)...");
    let result = repo.execute_command::<InventoryItem, ReserveItem>(
        item_id,
        &ReserveItem { quantity: 40 },
        &(),
    );

    match result.await {
        Err(OptimisticCommandError::Aggregate(InventoryError::InsufficientStock {
            requested,
            available,
        })) => {
            println!("   Correctly rejected: requested {requested}, available {available}");
        }
        Ok(()) => {
            println!("   Unexpectedly succeeded!");
        }
        Err(e) => {
            println!("   Error: {e}");
        }
    }
}

/// Print the summary of key takeaways.
fn print_summary() {
    println!("\n=== Example Complete ===");
    println!("\nKey takeaways:");
    println!("  1. Optimistic concurrency is enabled by default");
    println!("  2. Conflicts are detected when the stream version changes between load and commit");
    println!("  3. Use execute_with_retry() for automatic retry on ConcurrencyConflict");
    println!(
        "  4. Business rules are always evaluated against fresh state after conflict resolution"
    );
    println!("  5. Use .without_concurrency_checking() for single-writer scenarios");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Optimistic Concurrency Example ===\n");

    let (mut repo1, item1_id) = part1_basic_usage().await?;
    part2_conflict_detection(&mut repo1, &item1_id).await?;

    let (mut repo2, item2_id) = part3_retry_pattern().await?;
    part4_business_rules(&mut repo2, &item2_id).await;

    print_summary();

    Ok(())
}
