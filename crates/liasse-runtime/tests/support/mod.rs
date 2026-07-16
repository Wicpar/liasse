//! Shared fixtures for the runtime integration tests (MemoryStore-backed).
//!
//! Each integration-test binary uses a different subset of these fixtures, so
//! the unused-per-binary items are expected.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_ident::InstanceId;
use liasse_runtime::{Engine, FixedGenerators, Precision};
use liasse_store::MemoryStore;

/// A fixed micro-precision instant used as the deterministic `now()` sample.
pub const NOW_MICROS: i128 = 1_700_000_000_000_000;

/// A fresh deterministic generator: `now()` fixed, seeds from zero.
#[must_use]
pub fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW_MICROS, Precision::Micros)
}

/// A fresh in-memory store for `instance`.
#[must_use]
pub fn store(instance: &str) -> MemoryStore {
    MemoryStore::new(InstanceId::new(instance))
}

/// Load a definition into a fresh store, panicking (test failure) on any error.
#[must_use]
pub fn load(instance: &str, definition: &str) -> Engine<MemoryStore> {
    let mut generator = generator();
    match Engine::load(store(instance), definition, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

/// The §3.2 tasks application, with an `all_tasks` inspection view exposing the
/// generated fields the tests assert on.
pub const TASKS: &str = r#"{
  "$liasse": 1
  "$app": "example.tasks@1.0.0"
  "$model": {
    "tasks": {
      "$key": "id"
      "id": "uuid = uuid()"
      "title": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A title is required"]
      }
      "done": "bool = false"
      "created_at": "timestamp = now()"
      "$mut": { "complete": ".done = true" }
    }
    "open_tasks": { "$view": ".tasks[:t | !t.done] { id, title }" }
    "all_tasks": { "$view": ".tasks { id, title, done, created_at }" }
    "$mut": { "add_task": ".tasks + { title: @title }" }
  }
}"#;

/// A bank application exercising the §5 dynamic rules (normalization, row check,
/// uniqueness, references) and §8 atomic admission (multi-statement all-or-
/// nothing, assertions, `return` from committed state).
pub const BANK: &str = r#"{
  "$liasse": 1
  "$app": "example.bank@1.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "$unique": ["email"]
      "id": "text"
      "email": { "$type": "text", "$normalize": "string.lower(string.trim(.))" }
      "balance": "int = 0"
      "$check": [".balance >= 0", "No overdraft"]
      "$mut": {
        "withdraw({ amount: int })": [
          "assert(.balance >= @amount, 'Insufficient funds')"
          ".balance = .balance - @amount"
        ]
        "deposit({ amount: int })": ".balance = .balance + @amount"
        "bump({ by: int })": [
          ".balance = .balance + @by"
          ".email = @email"
          "assert(.balance <= 100, 'cap exceeded')"
        ]
      }
    }
    "memberships": {
      "$key": "id"
      "id": "text"
      "account": { "$ref": "/accounts" }
    }
    "people": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "$mut": { "rename": [".name = @name", "return . { id, name }"] }
    }
    "all_accounts": { "$view": ".accounts { id, email, balance }" }
    "$mut": {
      "open_account": ".accounts + { id: @id, email: @email }"
      "set_balance({ id: text, amount: int })": ".accounts[@id].balance = @amount"
      "add_membership": ".memberships + { id: @id, account: @account }"
      "add_person": ".people + { id: @id, name: @name }"
    }
  }
}"#;

/// A seeded application: `$data` companies exercise §9.1 seed admission through
/// the full rule pipeline (defaults, normalization, checks).
pub const SEEDED: &str = r#"{
  "$liasse": 1
  "$app": "example.seeded@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A name is required"]
      }
      "tier": "int = 1"
    }
    "all_companies": { "$view": ".companies { id, name, tier }" }
  }
  "$data": {
    "companies": {
      "acme": { "name": "  Acme  " }
      "globex": { "name": "Globex", "tier": 3 }
    }
  }
}"#;

/// A §14 buckets application exercising lifecycle buckets over the engine's
/// virtual clock: `sessions` expire at an upper bound (short-form `$until`),
/// `licenses` begin at an explicit `$from` and never expire, and `reservations`
/// carry an explicit finite `[from, until)` whose validity is enforced.
pub const BUCKETS: &str = r#"{
  "$liasse": 1
  "$app": "example.buckets@1.0.0"
  "$model": {
    "sessions": {
      "$key": "id"
      "$bucket": ".expires_at"
      "id": "text"
      "expires_at": "timestamp"
    }
    "licenses": {
      "$key": "id"
      "$bucket": { "$from": ".starts_at" }
      "id": "text"
      "starts_at": "timestamp"
    }
    "reservations": {
      "$key": "id"
      "$bucket": { "$from": ".starts_at", "$until": ".ends_at" }
      "id": "text"
      "starts_at": "timestamp"
      "ends_at": "timestamp"
    }
    "active_sessions": { "$view": ".sessions { id, expires_at }" }
    "active_licenses": { "$view": ".licenses { id, starts_at }" }
    "active_reservations": { "$view": ".reservations { id }" }
    "$mut": {
      "open_session": ".sessions + { id: @id, expires_at: @expires_at }"
      "open_license": ".licenses + { id: @id, starts_at: @starts_at }"
      "reserve": ".reservations + { id: @id, starts_at: @starts_at, ends_at: @ends_at }"
      "sessions_at({ t: timestamp })": "return .sessions.$at(@t) { id }"
      "sessions_between({ a: timestamp, b: timestamp })": "return .sessions.$between(@a, @b) { id }"
      "sessions_all": "return .sessions.$all { id }"
    }
  }
}"#;

/// The seeded application with an invalid seed row (a blank name failing the
/// size check), for the seed-rejection case.
pub const SEEDED_INVALID: &str = r#"{
  "$liasse": 1
  "$app": "example.seeded@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": {
        "$type": "text"
        "$normalize": "string.trim(.)"
        "$check": ["size(.) > 0", "A name is required"]
      }
      "tier": "int = 1"
    }
  }
  "$data": {
    "companies": {
      "blank": { "name": "   " }
    }
  }
}"#;
