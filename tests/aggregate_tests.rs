//! Integration tests for aggregate behavior using the [`TestFramework`].
//! Run with `cargo test --features test-util`.

#[cfg(feature = "test-util")]
mod with_test_util {
    use event_sourcing::test::TestFramework;
    use event_sourcing::{
        Aggregate, Codec, DomainEvent, Handle, PersistableEvent, ProjectionEvent, SerializableEvent,
    };
    use serde::{Deserialize, Serialize};

    // ============================================================================
    // Test Domain: Bank Account
    // ============================================================================

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct MoneyDeposited {
        amount: u64,
    }

    impl DomainEvent for MoneyDeposited {
        const KIND: &'static str = "money-deposited";
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct MoneyWithdrawn {
        amount: u64,
    }

    impl DomainEvent for MoneyWithdrawn {
        const KIND: &'static str = "money-withdrawn";
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct AccountOpened {
        initial_balance: u64,
    }

    impl DomainEvent for AccountOpened {
        const KIND: &'static str = "account-opened";
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum BankAccountEvent {
        Opened(AccountOpened),
        Deposited(MoneyDeposited),
        Withdrawn(MoneyWithdrawn),
    }

    impl SerializableEvent for BankAccountEvent {
        fn to_persistable<C: Codec, M>(
            self,
            codec: &C,
            metadata: M,
        ) -> Result<PersistableEvent<M>, C::Error> {
            match self {
                Self::Opened(e) => Ok(PersistableEvent {
                    kind: AccountOpened::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
                Self::Deposited(e) => Ok(PersistableEvent {
                    kind: MoneyDeposited::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
                Self::Withdrawn(e) => Ok(PersistableEvent {
                    kind: MoneyWithdrawn::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
            }
        }
    }

    impl ProjectionEvent for BankAccountEvent {
        const EVENT_KINDS: &'static [&'static str] = &[
            AccountOpened::KIND,
            MoneyDeposited::KIND,
            MoneyWithdrawn::KIND,
        ];

        fn from_stored<C: Codec>(kind: &str, data: &[u8], codec: &C) -> Result<Self, C::Error> {
            match kind {
                "account-opened" => Ok(Self::Opened(codec.deserialize(data)?)),
                "money-deposited" => Ok(Self::Deposited(codec.deserialize(data)?)),
                "money-withdrawn" => Ok(Self::Withdrawn(codec.deserialize(data)?)),
                _ => panic!("Unknown event kind: {kind}"),
            }
        }
    }

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct BankAccount {
        balance: u64,
        is_open: bool,
    }

    // Hand-written aggregates only need to implement Aggregate::apply directly.
    // The Apply<E> trait is only required when using #[derive(Aggregate)].
    impl Aggregate for BankAccount {
        const KIND: &'static str = "bank-account";
        type Event = BankAccountEvent;
        type Error = String;
        type Id = String;

        fn apply(&mut self, event: Self::Event) {
            match event {
                BankAccountEvent::Opened(e) => {
                    self.is_open = true;
                    self.balance = e.initial_balance;
                }
                BankAccountEvent::Deposited(e) => {
                    self.balance += e.amount;
                }
                BankAccountEvent::Withdrawn(e) => {
                    self.balance -= e.amount;
                }
            }
        }
    }

    // Commands
    struct OpenAccount {
        initial_balance: u64,
    }

    struct Deposit {
        amount: u64,
    }

    struct Withdraw {
        amount: u64,
    }

    impl Handle<OpenAccount> for BankAccount {
        fn handle(&self, command: &OpenAccount) -> Result<Vec<Self::Event>, Self::Error> {
            if self.is_open {
                return Err("account already open".to_string());
            }
            Ok(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: command.initial_balance,
            })])
        }
    }

    impl Handle<Deposit> for BankAccount {
        fn handle(&self, command: &Deposit) -> Result<Vec<Self::Event>, Self::Error> {
            if !self.is_open {
                return Err("account not open".to_string());
            }
            if command.amount == 0 {
                return Err("amount must be positive".to_string());
            }
            Ok(vec![BankAccountEvent::Deposited(MoneyDeposited {
                amount: command.amount,
            })])
        }
    }

    impl Handle<Withdraw> for BankAccount {
        fn handle(&self, command: &Withdraw) -> Result<Vec<Self::Event>, Self::Error> {
            if !self.is_open {
                return Err("account not open".to_string());
            }
            if command.amount == 0 {
                return Err("amount must be positive".to_string());
            }
            if command.amount > self.balance {
                return Err("insufficient funds".to_string());
            }
            Ok(vec![BankAccountEvent::Withdrawn(MoneyWithdrawn {
                amount: command.amount,
            })])
        }
    }

    // ============================================================================
    // Tests
    // ============================================================================

    type AccountTest = TestFramework<BankAccount>;

    #[test]
    fn open_account_produces_event() {
        AccountTest::new()
            .given_no_previous_events()
            .when(&OpenAccount {
                initial_balance: 100,
            })
            .then_expect_events(&[BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })]);
    }

    #[test]
    fn cannot_open_already_open_account() {
        AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 0,
            })])
            .when(&OpenAccount {
                initial_balance: 100,
            })
            .then_expect_error_message("already open");
    }

    #[test]
    fn deposit_increases_balance() {
        AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })])
            .when(&Deposit { amount: 50 })
            .then_expect_events(&[BankAccountEvent::Deposited(MoneyDeposited { amount: 50 })]);
    }

    #[test]
    fn cannot_deposit_to_closed_account() {
        AccountTest::new()
            .given_no_previous_events()
            .when(&Deposit { amount: 50 })
            .then_expect_error_message("not open");
    }

    #[test]
    fn withdraw_decreases_balance() {
        AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })])
            .when(&Withdraw { amount: 30 })
            .then_expect_events(&[BankAccountEvent::Withdrawn(MoneyWithdrawn { amount: 30 })]);
    }

    #[test]
    fn cannot_withdraw_more_than_balance() {
        AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })])
            .when(&Withdraw { amount: 150 })
            .then_expect_error_message("insufficient funds");
    }

    #[test]
    fn cannot_withdraw_from_closed_account() {
        AccountTest::new()
            .given_no_previous_events()
            .when(&Withdraw { amount: 50 })
            .then_expect_error_message("not open");
    }

    #[test]
    fn state_is_rebuilt_from_event_history() {
        // Verify that given() properly rebuilds state from events
        AccountTest::new()
            .given(vec![
                BankAccountEvent::Opened(AccountOpened {
                    initial_balance: 100,
                }),
                BankAccountEvent::Deposited(MoneyDeposited { amount: 50 }),
                BankAccountEvent::Withdrawn(MoneyWithdrawn { amount: 30 }),
            ])
            // Balance should be 100 + 50 - 30 = 120
            // Withdrawing 120 should succeed
            .when(&Withdraw { amount: 120 })
            .then_expect_events(&[BankAccountEvent::Withdrawn(MoneyWithdrawn { amount: 120 })]);
    }

    #[test]
    fn and_allows_building_complex_state() {
        AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })])
            .and(vec![BankAccountEvent::Deposited(MoneyDeposited {
                amount: 200,
            })])
            .and(vec![BankAccountEvent::Withdrawn(MoneyWithdrawn {
                amount: 50,
            })])
            // Balance: 100 + 200 - 50 = 250
            .when(&Withdraw { amount: 250 })
            .then_expect_events(&[BankAccountEvent::Withdrawn(MoneyWithdrawn { amount: 250 })]);
    }

    #[test]
    fn inspect_result_allows_custom_assertions() {
        let result = AccountTest::new()
            .given(vec![BankAccountEvent::Opened(AccountOpened {
                initial_balance: 100,
            })])
            .when(&Deposit { amount: 50 })
            .inspect_result();

        assert!(result.is_ok());
        let events = result.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            BankAccountEvent::Deposited(e) => assert_eq!(e.amount, 50),
            _ => panic!("Expected Deposited event"),
        }
    }
}

#[cfg(not(feature = "test-util"))]
#[test]
fn test_util_feature_is_required() {
    panic!(
        "Integration tests require the `test-util` feature. \
         Run `cargo test --features test-util` to execute them."
    );
}
