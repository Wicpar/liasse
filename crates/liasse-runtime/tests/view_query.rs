#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §10.1/§11.1 param- and actor-aware view reads through [`Engine::view_with`].
//!
//! A `$public` surface `$view` declares `$params` read as `@name`; an omitted
//! argument takes the declared default and a supplied one overrides it (§10.1,
//! §8.3). A role surface `$view` reads `$actor`, bound to the row the query's
//! actor key names, so the view filters to that actor's own rows (§11.1). Every
//! expectation is re-derived from the surface declarations, not the engine's own
//! answer.

mod support;

use liasse_runtime::{Engine, ViewQuery, Value};
use liasse_value::Text;
use support::{generator, store};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A package with a public param view (`roster`, filtering on `@archived`) and a
/// role view (`self.me`, filtering on `$actor.id`). Two seeded accounts: `alice`
/// active, `bob` archived.
const APP: &str = r#"{
  "$liasse": 1
  "$app": "t.vq@1.0.0"
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text", "archived": "bool = false" }
    "$public": {
      "roster": {
        "$params": { "archived": "bool = false" }
        "$view": ".accounts[:a | a.archived == @archived] { id, name }"
      }
    }
    "$auth": {
      "session": {
        "$credential": "text"
        "$verify": "$credential"
        "$actor": "/accounts[$proof.account]"
      }
    }
    "$roles": {
      "self": {
        "$members": ".accounts[:a | a.id == $actor.id]"
        "me": { "$view": ".accounts[:a | a.id == $actor.id] { id, name }" }
      }
    }
  }
  "$data": {
    "accounts": { "alice": { "name": "Alice" }, "bob": { "name": "Bob", "archived": true } }
  }
}"#;

fn app() -> Engine<liasse_store::MemoryStore> {
    let mut generator = generator();
    match Engine::load(store("vq"), APP, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("load failed: {error}"),
    }
}

/// The `id` fields of a view result, in result order.
fn ids(engine: &Engine<liasse_store::MemoryStore>, name: &str, query: &ViewQuery) -> Vec<String> {
    let head = engine.head();
    let result = engine
        .view_with(name, head, query)
        .expect("view read ok")
        .unwrap_or_else(|| panic!("view `{name}` is declared"));
    result
        .rows()
        .iter()
        .map(|row| match row.field("id").expect("id cell") {
            Value::Text(text) => text.as_str().to_owned(),
            other => panic!("id is not text: {other:?}"),
        })
        .collect()
}

/// §10.1/§8.3: an omitted `@archived` takes its declared default `false`, so the
/// public roster shows only the active account.
#[test]
fn omitted_param_takes_declared_default() {
    let engine = app();
    assert_eq!(ids(&engine, "public.roster", &ViewQuery::new()), vec!["alice".to_owned()]);
}

/// §10.1: a supplied `@archived` overrides the default, selecting the archived
/// account instead — proving the argument reaches `Environment::param`.
#[test]
fn supplied_param_overrides_default() {
    let engine = app();
    let query = ViewQuery::new().param("archived", Value::Bool(true));
    assert_eq!(ids(&engine, "public.roster", &query), vec!["bob".to_owned()]);
}

/// §11.1: a role `$view` reads `$actor`, bound to the row the query's actor key
/// names, so `self.me` returns exactly that actor's own account.
#[test]
fn role_view_binds_actor_from_query() {
    let engine = app();
    let alice = ViewQuery::new().actor(text("alice"));
    assert_eq!(ids(&engine, "self.me", &alice), vec!["alice".to_owned()]);

    let bob = ViewQuery::new().actor(text("bob"));
    assert_eq!(ids(&engine, "self.me", &bob), vec!["bob".to_owned()]);
}

/// A read of an undeclared view name yields `None`, not an error.
#[test]
fn undeclared_view_is_none() {
    let engine = app();
    assert!(engine.view_with("public.nope", engine.head(), &ViewQuery::new()).expect("ok").is_none());
}

/// §10.1: the declared `$params` of a surface view are exposed by name and scalar
/// type, so a host can decode a client's `view` arguments against the same
/// contract the runtime type-checked. The `roster` surface declares one `bool`
/// parameter `archived`; the role `self.me` declares none.
#[test]
fn surface_view_params_are_exposed_by_name_and_type() {
    let engine = app();
    assert_eq!(
        engine.surface_view_params("public.roster"),
        vec![("archived".to_owned(), liasse_value::Type::Bool)]
    );
    assert!(engine.surface_view_params("self.me").is_empty());
    assert!(engine.surface_view_params("public.nope").is_empty());
}

/// A bucketed collection under a parameterized surface `$view` (§14.1, §10.1): a
/// `.$at(@t)` temporal selector reads its instant from a supplied argument, so the
/// param- and actor-aware read observes exactly the rows active at that instant —
/// present at the expiry-bounded interval, absent one microsecond past it.
const BUCKET_APP: &str = r#"{
  "$liasse": 1
  "$app": "t.vqb@1.0.0"
  "$model": {
    "sessions": { "$key": "id", "$bucket": ".expires_at", "id": "text", "expires_at": "timestamp" }
    "$public": { "at": { "$params": { "t": "timestamp" }, "$view": ".sessions.$at(@t) { id }" } }
  }
  "$data": { "sessions": { "s1": { "expires_at": "1767312000000000" } } }
}"#;

#[test]
fn parameterized_bucket_view_reads_the_argument_instant() {
    let mut generator = generator();
    let engine = Engine::load(store("vqb"), BUCKET_APP, &mut generator).expect("load");
    let head = engine.head();
    let at = |micros: i128| {
        let t = Value::Timestamp(liasse_value::Timestamp::new(micros, liasse_value::Precision::Micros));
        let result = engine
            .view_with("public.at", head, &ViewQuery::new().param("t", t))
            .expect("view read ok")
            .expect("view declared");
        result.rows().len()
    };
    // Active from creation through the exclusive `expires_at` upper bound.
    assert_eq!(at(1_767_311_999_999_999), 1, "active just before expiry");
    assert_eq!(at(1_767_312_000_000_000), 0, "gone at the exclusive upper bound");
}
