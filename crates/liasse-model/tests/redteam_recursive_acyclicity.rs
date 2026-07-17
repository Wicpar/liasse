//! §10.5 recursive-surface acyclicity / strict-descendant check.
//!
//! §10.5: "`$through` yields strict descendants of the current row" and "The
//! checker verifies descendant shape, acyclicity, identity, and predicate
//! types." A `$through` that selects the whole top-level collection includes the
//! current row and its siblings, so the coverage relation is neither strict nor
//! acyclic (every row would recursively cover itself and every peer). The
//! checker MUST reject such a package.
//!
//! The corpus red case `recursive-through-must-be-strict-descendants` uses the
//! SAME invalid `$through: "/companies[:c]"` but omits the `subcompanies` field
//! the `$field` names, so it also trips a MISSING-`$field` rejection. This test
//! supplies a valid `$field` (`subcompanies: { $like: "^" }`, exactly as the
//! passing coverage-nests case declares it), isolating the acyclicity rule: the
//! ONLY §10.5 violation left is that `$through` does not yield strict descendants.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::build;
use liasse_model::code;

/// The valid coverage model (`$through: ".subcompanies"`) is the control: it
/// loads, proving the surrounding shape (roles, auth, `$like` self-nesting) is
/// well-formed and the only variable under test is the `$through` relation.
#[test]
fn strict_descendant_through_control_loads() {
    let def = model(r#".subcompanies"#);
    let built = build(&def);
    built.expect_ok();
}

/// §10.5: `$through: "/companies[:c]"` selects every company — the current row,
/// its siblings, and its ancestors — so the recursive coverage relation is
/// cyclic (a company covers itself). The checker verifies "acyclicity", so this
/// MUST be rejected. `$field` is a valid field, so a rejection here can only be
/// the acyclicity / strict-descendant rule.
#[test]
fn cyclic_through_whole_collection_rejected() {
    let def = model(r#"/companies[:c]"#);
    let built = build(&def);
    assert!(
        built.result.is_err(),
        "SPEC VIOLATION (§10.5): a `$recursive` `$through` that selects the whole \
         `/companies` collection is not a strict-descendant (acyclic) relation — a company \
         would recursively cover itself — yet the model builder accepted it."
    );
    // The rejection is the §10.5 surface check, and its message names the
    // strict-descendant / acyclicity rule — so this is the RIGHT reason, not an
    // incidental failure. `$field` is valid, so the missing-field rule cannot fire.
    assert!(built.has_code(code::SURFACE), "the §10.5 rejection is a surface diagnostic: {}", built.rendered());
    assert!(
        built.rendered().contains("strict descendants"),
        "the diagnostic reports the strict-descendant / acyclicity rule: {}",
        built.rendered()
    );
}

/// Build the coverage model with the given `$through` expression.
fn model(through: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.rec_cycle@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text" }},
            "companies": {{
              "$key": "id",
              "id": "text",
              "name": "text",
              "subcompanies": {{ "$like": "^" }},
              "members": {{
                "$key": "account",
                "account": {{ "$ref": "/accounts" }},
                "admin": "bool = false"
              }},
              "$roles": {{
                "admin": {{
                  "$auth": "token",
                  "$members": ".members[:m | m.admin].account",
                  "company": {{
                    "$view": ". {{ id, name }}",
                    "$recursive": {{
                      "$field": "subcompanies",
                      "$through": "{through}",
                      "$bind": "child"
                    }}
                  }}
                }}
              }}
            }},
            "$auth": {{
              "token": {{
                "$credential": "text",
                "$verify": "$credential",
                "$actor": "/accounts[$proof]"
              }}
            }}
          }}
        }}"#
    )
}
