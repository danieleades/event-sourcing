//! Subscription Billing Example – Command/Query split with cross-aggregate
//! projection
//!
//! This example demonstrates a realistic CQRS flow using the primitives
//! provided by the `sourcery` crate:
//! - **Subscription aggregate** drives the customer lifecycle
//!   (activation/cancellation).
//! - **Invoice aggregate** handles billing and payments for a specific customer
//!   invoice.
//! - **`CustomerBillingProjection`** consumes events from *both* aggregates to
//!   maintain an operational dashboard: plan status, outstanding balance, audit
//!   metadata.
//!
//! The example emphasises CQRS good practice:
//! - Aggregates emit domain-focused events with no persistence artefacts.
//! - Commands include tracing metadata (`causation_id`, `correlation_id`,
//!   `user_id`), making downstream projections observable.
//! - The query-side projection is idempotent and could be materialised
//!   asynchronously. (For the demo we rebuild it in-process; in production it
//!   would run in its own worker.)
//! - The command side consults the read model before allowing a risky operation
//!   (cancelling a subscription while money is outstanding).

use std::{collections::HashMap, fmt};

use serde::{Deserialize, Serialize};
use sourcery::{
    Aggregate, Apply, ApplyProjection, DomainEvent, Handle, Projection, Repository,
    store::{JsonCodec, inmemory},
};

// =============================================================================
// Shared domain types
// =============================================================================

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub struct EventMetadata {
    correlation_id: String,
    user_id: String,
}

// =============================================================================
// Subscription Aggregate
// =============================================================================

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(
    id = String,
    error = String,
    events(SubscriptionStarted, SubscriptionCancelled),
    kind = "subscription"
)]
pub struct Subscription {
    status: SubscriptionStatus,
    active_plan: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum SubscriptionStatus {
    Active,
    Cancelled,
    #[default]
    Inactive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscriptionStarted {
    pub plan_name: String,
    pub activated_at: String,
}

impl DomainEvent for SubscriptionStarted {
    const KIND: &'static str = "billing.subscription.started";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscriptionCancelled {
    pub reason: String,
    pub cancelled_at: String,
}

impl DomainEvent for SubscriptionCancelled {
    const KIND: &'static str = "billing.subscription.cancelled";
}

impl Apply<SubscriptionStarted> for Subscription {
    fn apply(&mut self, event: &SubscriptionStarted) {
        self.status = SubscriptionStatus::Active;
        self.active_plan = Some(event.plan_name.clone());
    }
}

impl Apply<SubscriptionCancelled> for Subscription {
    fn apply(&mut self, _event: &SubscriptionCancelled) {
        self.status = SubscriptionStatus::Cancelled;
    }
}

// Command structs for Subscription aggregate
#[derive(Debug)]
pub struct StartSubscription {
    pub plan_name: String,
    pub activated_at: String,
}

#[derive(Debug)]
pub struct CancelSubscription {
    pub reason: String,
    pub cancelled_at: String,
}

// Handle<C> implementations for each command
impl Handle<StartSubscription> for Subscription {
    fn handle(&self, command: &StartSubscription) -> Result<Vec<Self::Event>, Self::Error> {
        if self.status == SubscriptionStatus::Active {
            return Err("subscription already active".into());
        }
        if self.status == SubscriptionStatus::Cancelled {
            return Err("cancelled subscription cannot be restarted".into());
        }
        Ok(vec![
            SubscriptionStarted {
                plan_name: command.plan_name.clone(),
                activated_at: command.activated_at.clone(),
            }
            .into(),
        ])
    }
}

impl Handle<CancelSubscription> for Subscription {
    fn handle(&self, command: &CancelSubscription) -> Result<Vec<Self::Event>, Self::Error> {
        if self.status != SubscriptionStatus::Active {
            return Err("only active subscriptions can be cancelled".into());
        }
        Ok(vec![
            SubscriptionCancelled {
                reason: command.reason.clone(),
                cancelled_at: command.cancelled_at.clone(),
            }
            .into(),
        ])
    }
}

// =============================================================================
// Invoice Aggregate
// =============================================================================

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvoiceId {
    pub customer_id: String,
    pub invoice_number: String,
}

impl InvoiceId {
    pub fn new(customer_id: String, invoice_number: impl Into<String>) -> Self {
        Self {
            customer_id,
            invoice_number: invoice_number.into(),
        }
    }
}

impl fmt::Display for InvoiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.customer_id, self.invoice_number)
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Aggregate)]
#[aggregate(
    id = String,
    error = String,
    events(InvoiceIssued, PaymentRecorded, InvoiceSettled),
    kind = "invoice"
)]
pub struct Invoice {
    issued: bool,
    settled: bool,
    customer_id: Option<String>,
    amount_cents: i64,
    paid_cents: i64,
    due_date: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvoiceIssued {
    pub customer_id: String,
    pub amount_cents: i64,
    pub due_date: String,
}

impl DomainEvent for InvoiceIssued {
    const KIND: &'static str = "billing.invoice.issued";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaymentRecorded {
    pub customer_id: String,
    pub amount_cents: i64,
}

impl DomainEvent for PaymentRecorded {
    const KIND: &'static str = "billing.invoice.payment_recorded";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvoiceSettled {
    pub customer_id: String,
}

impl DomainEvent for InvoiceSettled {
    const KIND: &'static str = "billing.invoice.settled";
}

impl Apply<InvoiceIssued> for Invoice {
    fn apply(&mut self, event: &InvoiceIssued) {
        self.issued = true;
        self.customer_id = Some(event.customer_id.clone());
        self.amount_cents = event.amount_cents;
        self.due_date = Some(event.due_date.clone());
    }
}

impl Apply<PaymentRecorded> for Invoice {
    fn apply(&mut self, event: &PaymentRecorded) {
        self.paid_cents += event.amount_cents;
    }
}

impl Apply<InvoiceSettled> for Invoice {
    fn apply(&mut self, _event: &InvoiceSettled) {
        self.settled = true;
    }
}

// Command structs for Invoice aggregate
#[derive(Debug)]
pub struct IssueInvoice {
    pub customer_id: String,
    pub amount_cents: i64,
    pub due_date: String,
}

#[derive(Debug)]
pub struct RecordPayment {
    pub amount_cents: i64,
}

// Handle<C> implementations for each command
impl Handle<IssueInvoice> for Invoice {
    fn handle(&self, command: &IssueInvoice) -> Result<Vec<Self::Event>, Self::Error> {
        if self.issued {
            return Err("invoice already issued".into());
        }
        if command.amount_cents <= 0 {
            return Err("invoice amount must be positive".into());
        }
        Ok(vec![
            InvoiceIssued {
                customer_id: command.customer_id.clone(),
                amount_cents: command.amount_cents,
                due_date: command.due_date.clone(),
            }
            .into(),
        ])
    }
}

impl Handle<RecordPayment> for Invoice {
    fn handle(&self, command: &RecordPayment) -> Result<Vec<Self::Event>, Self::Error> {
        if !self.issued {
            return Err("invoice not issued yet".into());
        }
        if self.settled {
            return Err("invoice already settled".into());
        }
        if command.amount_cents <= 0 {
            return Err("payment must be positive".into());
        }
        let customer = self
            .customer_id
            .clone()
            .ok_or_else(|| "invoice missing customer context".to_string())?;
        let outstanding = self.amount_cents - self.paid_cents;
        if command.amount_cents > outstanding {
            return Err(format!(
                "payment ({}) exceeds outstanding balance ({outstanding})",
                command.amount_cents
            ));
        }

        let mut events = vec![
            PaymentRecorded {
                customer_id: customer.clone(),
                amount_cents: command.amount_cents,
            }
            .into(),
        ];

        if command.amount_cents == outstanding {
            events.push(
                InvoiceSettled {
                    customer_id: customer,
                }
                .into(),
            );
        }

        Ok(events)
    }
}

// =============================================================================
// Projection: Customer Billing Dashboard
// =============================================================================

#[derive(Debug, Default, Clone)]
pub struct CustomerSnapshot {
    pub active_plan: Option<String>,
    pub is_active: bool,
    pub outstanding_balance_cents: i64,
    pub last_invoice_due: Option<String>,
    pub last_correlation_id: String,
    pub last_updated_by: String,
}

#[derive(Debug, Default)]
pub struct CustomerBillingProjection {
    customers: HashMap<String, CustomerSnapshot>,
}

impl Projection for CustomerBillingProjection {
    type Id = String;
    type Metadata = EventMetadata;
}

impl CustomerBillingProjection {
    fn touch_customer(&mut self, id: String) -> &mut CustomerSnapshot {
        self.customers.entry(id).or_default()
    }

    #[must_use]
    pub fn customer(&self, id: &str) -> Option<&CustomerSnapshot> {
        self.customers.get(id)
    }

    /// Iterate customers for reporting.
    pub fn customers(&self) -> impl Iterator<Item = (&String, &CustomerSnapshot)> {
        self.customers.iter()
    }
}

impl ApplyProjection<SubscriptionStarted> for CustomerBillingProjection {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &SubscriptionStarted,
        metadata: &Self::Metadata,
    ) {
        let customer_id = aggregate_id;
        let snapshot = self.touch_customer(customer_id.to_owned());
        snapshot.active_plan = Some(event.plan_name.clone());
        snapshot.is_active = true;
        snapshot
            .last_correlation_id
            .clone_from(&metadata.correlation_id);
        snapshot.last_updated_by.clone_from(&metadata.user_id);
    }
}

impl ApplyProjection<SubscriptionCancelled> for CustomerBillingProjection {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        _event: &SubscriptionCancelled,
        metadata: &Self::Metadata,
    ) {
        // aggregate_id already has the "subscription::" prefix stripped
        let customer_id = aggregate_id;
        let snapshot = self.touch_customer(customer_id.to_owned());
        snapshot.is_active = false;
        snapshot
            .last_correlation_id
            .clone_from(&metadata.correlation_id);
        snapshot.last_updated_by.clone_from(&metadata.user_id);
    }
}

impl ApplyProjection<InvoiceIssued> for CustomerBillingProjection {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &InvoiceIssued,
        metadata: &Self::Metadata,
    ) {
        let snapshot = self.touch_customer(event.customer_id.clone());
        snapshot.outstanding_balance_cents += event.amount_cents;
        snapshot.last_invoice_due = Some(event.due_date.clone());
        snapshot
            .last_correlation_id
            .clone_from(&metadata.correlation_id);
        snapshot.last_updated_by.clone_from(&metadata.user_id);
    }
}

impl ApplyProjection<PaymentRecorded> for CustomerBillingProjection {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &PaymentRecorded,
        metadata: &Self::Metadata,
    ) {
        let snapshot = self.touch_customer(event.customer_id.clone());
        snapshot.outstanding_balance_cents =
            (snapshot.outstanding_balance_cents - event.amount_cents).max(0);
        snapshot
            .last_correlation_id
            .clone_from(&metadata.correlation_id);
        snapshot.last_updated_by.clone_from(&metadata.user_id);
    }
}

impl ApplyProjection<InvoiceSettled> for CustomerBillingProjection {
    fn apply_projection(
        &mut self,
        _aggregate_id: &Self::Id,
        event: &InvoiceSettled,
        metadata: &Self::Metadata,
    ) {
        let snapshot = self.touch_customer(event.customer_id.clone());
        snapshot.outstanding_balance_cents = 0;
        snapshot
            .last_correlation_id
            .clone_from(&metadata.correlation_id);
        snapshot.last_updated_by.clone_from(&metadata.user_id);
    }
}

// =============================================================================
// Example usage
// =============================================================================

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store: inmemory::Store<String, JsonCodec, EventMetadata> = inmemory::Store::new(JsonCodec);
    let mut repository = Repository::new(store);

    let customer_id = String::from("ACME-001");
    let subscription_corr = format!("subscription/{}", customer_id.as_str());

    // Activate the subscription
    repository
        .execute_command::<Subscription, StartSubscription>(
            &customer_id,
            &StartSubscription {
                plan_name: "Pro Annual".into(),
                activated_at: "2024-10-01".into(),
            },
            &EventMetadata {
                correlation_id: subscription_corr.clone(),
                user_id: "crm-system".to_owned(),
            },
        )
        .await?;

    // Issue an invoice tied to the subscription lifecycle
    let invoice_id = InvoiceId::new(customer_id.clone(), "2024-INV-1001");
    let invoice_stream_id = invoice_id.to_string();
    let invoice_corr = format!("invoice/{}", invoice_id.invoice_number);

    repository
        .execute_command::<Invoice, IssueInvoice>(
            &invoice_stream_id,
            &IssueInvoice {
                customer_id: customer_id.clone(),
                amount_cents: 12_000,
                due_date: "2024-11-01".into(),
            },
            &EventMetadata {
                correlation_id: invoice_corr.clone(),
                user_id: "billing-engine".to_owned(),
            },
        )
        .await?;

    // Record a partial payment
    repository
        .execute_command::<Invoice, RecordPayment>(
            &invoice_stream_id,
            &RecordPayment {
                amount_cents: 5_000,
            },
            &EventMetadata {
                correlation_id: invoice_corr.clone(),
                user_id: "payments-service".to_owned(),
            },
        )
        .await?;

    // Record remaining balance
    repository
        .execute_command::<Invoice, RecordPayment>(
            &invoice_stream_id,
            &RecordPayment {
                amount_cents: 7_000,
            },
            &EventMetadata {
                correlation_id: invoice_corr,
                user_id: "payments-service".to_owned(),
            },
        )
        .await?;

    // Build the read model (would typically happen asynchronously)
    let billing_projection = repository
        .build_projection::<CustomerBillingProjection>()
        .event::<SubscriptionStarted>()
        .event::<SubscriptionCancelled>()
        .event::<InvoiceIssued>()
        .event::<PaymentRecorded>()
        .event::<InvoiceSettled>()
        .load()
        .await?;

    // Guard cancelling the subscription with the up-to-date read model
    if billing_projection
        .customer(&customer_id)
        .is_some_and(|snapshot| snapshot.outstanding_balance_cents == 0)
    {
        repository
            .execute_command::<Subscription, CancelSubscription>(
                &customer_id,
                &CancelSubscription {
                    reason: "customer requested cancellation".into(),
                    cancelled_at: "2024-12-31".into(),
                },
                &EventMetadata {
                    correlation_id: subscription_corr,
                    user_id: "crm-system".to_owned(),
                },
            )
            .await?;
    } else {
        println!("Subscription not cancelled – outstanding balance detected.");
    }

    let final_projection = repository
        .build_projection::<CustomerBillingProjection>()
        .event::<SubscriptionStarted>()
        .event::<SubscriptionCancelled>()
        .event::<InvoiceIssued>()
        .event::<PaymentRecorded>()
        .event::<InvoiceSettled>()
        .load()
        .await?;

    println!("=== Customer Billing Dashboard ===");
    for (customer, snapshot) in final_projection.customers() {
        println!("Customer: {customer}");
        println!(
            "  Active Plan: {}",
            snapshot.active_plan.as_deref().unwrap_or("none")
        );
        println!(
            "  Status: {}",
            if snapshot.is_active {
                "active"
            } else {
                "inactive"
            }
        );
        println!(
            "  Outstanding Balance: ${:.2}",
            snapshot.outstanding_balance_cents as f64 / 100.0
        );
        if let Some(due) = &snapshot.last_invoice_due {
            println!("  Last Invoice Due: {due}");
        }
        println!("  Last Correlation ID: {}", snapshot.last_correlation_id);
        println!();
    }

    Ok(())
}
