//! Demonstrates optimistic concurrency control for handling concurrent writes.
//!
//! This example shows how to use `Repository::with_optimistic_concurrency()` to
//! detect and handle conflicts when multiple writers try to modify the same
//! aggregate simultaneously.
//!
//! Run with: `cargo run --example optimistic_concurrency`

use event_sourcing::{
    Aggregate, Apply, DomainEvent, EventStore, Handle, InMemoryEventStore, JsonCodec,
    OptimisticCommandError, PersistableEvent, Repository,
};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Default, Serialize, Deserialize, event_sourcing::Aggregate)]
#[aggregate(id = String, error = InventoryError, events(ItemReserved, ItemRestocked))]
pub struct InventoryItem {
    available: u32,
}

#[derive(Debug, Clone)]
pub enum InventoryError {
    InsufficientStock { requested: u32, available: u32 },
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientStock {
                requested,
                available,
            } => {
                write!(
                    f,
                    "insufficient stock: requested {requested}, available {available}"
                )
            }
        }
    }
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
// Retry Helper
// =============================================================================

/// Result type alias for the retry helper.
type RetryResult<A, S, SS> = Result<
    usize,
    OptimisticCommandError<
        <A as event_sourcing::Aggregate>::Error,
        <S as event_sourcing::EventStore>::Position,
        <S as event_sourcing::EventStore>::Error,
        <<S as event_sourcing::EventStore>::Codec as event_sourcing::Codec>::Error,
        <SS as event_sourcing::SnapshotStore>::Error,
    >,
>;

/// Execute a command with automatic retry on concurrency conflicts.
///
/// This helper demonstrates a common pattern for handling optimistic concurrency:
/// when a conflict is detected, simply retry the operation with fresh state.
fn execute_with_retry<A, C, S, SS>(
    repo: &mut Repository<S, SS, event_sourcing::Optimistic>,
    id: &S::Id,
    command: &C,
    metadata: &S::Metadata,
    max_retries: usize,
) -> RetryResult<A, S, SS>
where
    A: event_sourcing::Aggregate<Id = S::Id> + Handle<C>,
    A::Id: std::fmt::Display,
    A::Event: event_sourcing::ProjectionEvent + event_sourcing::SerializableEvent + Clone,
    S: event_sourcing::EventStore,
    S::Metadata: Clone,
    SS: event_sourcing::SnapshotStore<Id = S::Id, Position = S::Position>,
{
    for attempt in 1..=max_retries {
        match repo.execute_command::<A, C>(id, command, metadata) {
            Ok(()) => return Ok(attempt),
            Err(OptimisticCommandError::Concurrency(conflict)) => {
                println!(
                    "     Attempt {attempt}: Conflict! Expected version {:?}, actual {:?}",
                    conflict.expected, conflict.actual
                );
            }
            Err(e) => return Err(e),
        }
    }
    // Final attempt
    repo.execute_command::<A, C>(id, command, metadata)
        .map(|()| max_retries + 1)
}

// =============================================================================
// Example Parts
// =============================================================================

type OptimisticRepo = Repository<
    InMemoryEventStore<String, JsonCodec, ()>,
    event_sourcing::NoSnapshots<String, u64>,
    event_sourcing::Optimistic,
>;

/// Part 1: Basic optimistic concurrency usage.
///
/// Demonstrates initializing inventory and making reservations without conflicts.
fn part1_basic_usage() -> Result<(OptimisticRepo, String), Box<dyn std::error::Error>> {
    println!("PART 1: Basic optimistic concurrency usage\n");

    let store: InMemoryEventStore<String, JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);
    let mut repo = Repository::new(store).with_optimistic_concurrency();

    let item_id = "SKU-001".to_string();

    // Initialize inventory
    println!("1. Restocking item with 100 units...");
    repo.execute_command::<InventoryItem, RestockItem>(
        &item_id,
        &RestockItem { quantity: 100 },
        &(),
    )?;

    let item: InventoryItem = repo.aggregate_builder().load(&item_id)?;
    println!("   Available: {}\n", item.available);

    // Normal reservation (no conflict)
    println!("2. Reserving 30 units (no concurrent modification)...");
    repo.execute_command::<InventoryItem, ReserveItem>(
        &item_id,
        &ReserveItem { quantity: 30 },
        &(),
    )?;

    let item: InventoryItem = repo.aggregate_builder().load(&item_id)?;
    println!("   Available: {}\n", item.available);

    Ok((repo, item_id))
}

/// Part 2: Conflict detection.
///
/// Demonstrates how concurrent modifications are detected when loading fresh state.
fn part2_conflict_detection(
    repo: &mut OptimisticRepo,
    item_id: &String,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("PART 2: Demonstrating conflict detection\n");

    // Simulate a concurrent modification by directly appending to the store
    // This is what would happen if another process/thread modified the aggregate
    println!("3. Simulating concurrent modification (another process reserves 20 units)...");
    repo.event_store_mut().append(
        InventoryItem::KIND,
        item_id,
        None, // Direct append without version check
        vec![PersistableEvent {
            kind: ItemReserved::KIND.to_string(),
            data: br#"{"quantity":20}"#.to_vec(),
            metadata: (),
        }],
    )?;

    let item: InventoryItem = repo.aggregate_builder().load(item_id)?;
    println!(
        "   Available after concurrent modification: {}\n",
        item.available
    );

    // Now try to reserve - this will succeed because execute_command loads fresh state
    // The conflict would only occur if we had pre-loaded state before the concurrent modification
    println!("4. Reserving 10 more units (loads fresh state, so no conflict)...");
    repo.execute_command::<InventoryItem, ReserveItem>(
        item_id,
        &ReserveItem { quantity: 10 },
        &(),
    )?;

    let item: InventoryItem = repo.aggregate_builder().load(item_id)?;
    println!("   Available: {}\n", item.available);

    Ok(())
}

/// Part 3: Retry pattern for handling conflicts.
///
/// Demonstrates how to use automatic retry logic when conflicts occur.
fn part3_retry_pattern() -> Result<(OptimisticRepo, String), Box<dyn std::error::Error>> {
    println!("PART 3: Retry pattern for handling conflicts\n");

    // Create a fresh store to demonstrate retry more clearly
    let store: InMemoryEventStore<String, JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);
    let mut repo = Repository::new(store).with_optimistic_concurrency();
    let item_id = "SKU-002".to_string();

    // Initialize
    repo.execute_command::<InventoryItem, RestockItem>(
        &item_id,
        &RestockItem { quantity: 50 },
        &(),
    )?;
    println!("5. Initialized SKU-002 with 50 units");

    // Inject a conflict before the retry helper runs
    // In a real system, this might be another service instance
    repo.event_store_mut().append(
        InventoryItem::KIND,
        &item_id,
        None,
        vec![PersistableEvent {
            kind: ItemReserved::KIND.to_string(),
            data: br#"{"quantity":5}"#.to_vec(),
            metadata: (),
        }],
    )?;
    println!("   Injected concurrent reservation of 5 units (simulating race condition)");

    println!("\n6. Attempting to reserve 10 units with retry logic...");
    let attempts = execute_with_retry::<InventoryItem, _, _, _>(
        &mut repo,
        &item_id,
        &ReserveItem { quantity: 10 },
        &(),
        3,
    )?;
    println!("   Succeeded on attempt {attempts}");

    let item: InventoryItem = repo.aggregate_builder().load(&item_id)?;
    println!(
        "   Final available: {} (50 - 5 - 10 = 35)\n",
        item.available
    );

    Ok((repo, item_id))
}

/// Part 4: Business rule enforcement.
///
/// Demonstrates that business rules are enforced against fresh state.
fn part4_business_rules(repo: &mut OptimisticRepo, item_id: &String) {
    println!("PART 4: Business rules with optimistic concurrency\n");

    println!("7. Attempting to reserve 40 units (should fail - only 35 available)...");
    let result = repo.execute_command::<InventoryItem, ReserveItem>(
        item_id,
        &ReserveItem { quantity: 40 },
        &(),
    );

    match result {
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
    println!("  1. Enable optimistic concurrency: repo.with_optimistic_concurrency()");
    println!("  2. Conflicts are detected when the stream version changes between load and commit");
    println!("  3. Handle OptimisticCommandError::Concurrency to implement retry logic");
    println!(
        "  4. Business rules are always evaluated against fresh state after conflict resolution"
    );
    println!("  5. Without optimistic concurrency (default), last-writer-wins semantics apply");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Optimistic Concurrency Example ===\n");

    let (mut repo1, item1_id) = part1_basic_usage()?;
    part2_conflict_detection(&mut repo1, &item1_id)?;

    let (mut repo2, item2_id) = part3_retry_pattern()?;
    part4_business_rules(&mut repo2, &item2_id);

    print_summary();

    Ok(())
}
