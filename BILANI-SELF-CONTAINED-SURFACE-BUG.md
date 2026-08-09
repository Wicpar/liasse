# Bilani blocker: a verified model loses its executable surface bindings

> **Delete this file in the fixing commit once the regression test passes.**

## Status

This is an actionable Liasse model and surface integration bug blocking Bilani from booting a generic Rust host from a serialized `.liasse` application state.

A `.liasse` artifact carries the complete definition and state. Liasse validates the definition's `$public`, `$roles`, `$view` and `$mut` declarations. After validation, however, `liasse_model::Surface` retains only:

```rust
pub struct Surface {
    pub name: DeclName,
    pub public: bool,
    pub calls: Vec<DeclName>,
}
```

The validated model discards the information needed to execute the surface:

- the role that owns a non-public surface;
- the surface view expression or compiled view binding;
- the mutation targeted by each external call;
- the receiver collection and receiver key arguments;
- the call parameter names and validated types;
- the authenticator and membership bindings associated with the role.

`liasse_surface::SurfaceRouterBuilder` then requires the host to provide that same information again through `SurfaceBinding`, `ViewBinding`, `CallBinding` and `Role`. A generic host cannot reconstruct the router from `Engine::model()` or the verified artifact without reparsing the raw package and duplicating Liasse's semantic work in application code.

## Minimal reproduction

Add this test under `crates/liasse-model/tests/surface_retention.rs`:

```rust
use liasse_diag::SourceMap;
use liasse_model::Model;

const ENABLE: &str = r#"{
  $liasse: 1
  $app: "repro.surface@1.0.0"
  $model: {
    enabled: "bool = false"
    $mut: {
      enable: ".enabled = true"
      disable: ".enabled = false"
    }
    $public: {
      controls: {
        $mut: { set: ".enable" }
      }
    }
  }
}"#;

const DISABLE: &str = r#"{
  $liasse: 1
  $app: "repro.surface@1.0.0"
  $model: {
    enabled: "bool = false"
    $mut: {
      enable: ".enabled = true"
      disable: ".enabled = false"
    }
    $public: {
      controls: {
        $mut: { set: ".disable" }
      }
    }
  }
}"#;

fn model(source: &str) -> Model {
    let mut sources = SourceMap::new();
    let id = sources.add_file("repro.liasse", source.to_owned());
    let document = liasse_syntax::parse_document(id, source).expect("definition parses");
    Model::build(&mut sources, id, &document).expect("definition validates")
}

#[test]
fn distinct_surface_behaviour_collapses_to_the_same_retained_surface() {
    let enable = model(ENABLE);
    let disable = model(DISABLE);

    let a = &enable.surfaces()[0];
    let b = &disable.surfaces()[0];

    assert_eq!(a.name.as_str(), b.name.as_str());
    assert_eq!(a.public, b.public);
    assert_eq!(
        a.calls.iter().map(|name| name.as_str()).collect::<Vec<_>>(),
        b.calls.iter().map(|name| name.as_str()).collect::<Vec<_>>()
    );

    // The definitions route `controls.set` to opposite mutations, but every
    // public field retained on Model::surfaces is identical. No consumer of the
    // verified Model can determine whether this call enables or disables.
    assert_eq!(a.calls[0].as_str(), "set");
}
```

Run it with:

```bash
cd /home/frederic/IdeaProjects/liasse-bilani-unblock
cargo test -p liasse-model --test surface_retention
```

The test passes today, which is the reproduction. Two definitions with opposite executable routing collapse to the same retained surface contract.

The same loss is visible directly in the implementation:

- `crates/liasse-model/src/surface.rs` validates each `$mut` reference in `surface_muts`, then stores only the external call name in `Surface.calls`.
- `crates/liasse-surface/src/binding.rs` documents that the model retains neither the reference nor the receiver split and requires the host to supply both again.
- `crates/liasse-surface/src/router/build.rs` can only check host-supplied bindings against names retained on the model. It cannot construct the bindings itself.

## Bilani impact

Bilani's Rust executable must be an application-agnostic Liasse host. The actual application is a provided `.liasse` serialized state file. No package, business data, surface routing table or application manifest may be compiled into the executable.

The current Bilani checkout therefore has a temporary `application.toml` that repeats every surface target, receiver split, argument type, authenticator coordinate and role membership view. Removing that manifest while this information is absent from Liasse would leave the generic host unable to route calls or watches safely.

Reparsing `artifact.liasse_json()` in Bilani is not a complete fix. It creates a second model reader in the host, still leaves typed external call contracts outside the retained model, and makes every Liasse host independently reproduce details Liasse already validated.

## Required behavior

A successfully built Liasse model must retain a complete executable surface plan, or `liasse-surface` must expose one constructor that builds the router directly from the verified model and its host components.

The authoritative retained plan must include at least:

- public scope or the exact owning role;
- the compiled surface view and its parameter contract;
- each external call's resolved mutation target;
- receiver path and key argument order;
- scalar and blob parameter names with their validated external types;
- role authenticator and membership bindings;
- enough source identity for useful diagnostics without requiring host-side reparsing.

The surface runtime may still accept host implementations for generic protocols. The host must not restate application routing that the package already declares.

## Acceptance criteria

- A Liasse regression demonstrates that the two definitions above produce distinguishable retained executable surface plans.
- A router can be constructed from a verified model without host-supplied mutation names, view names, receiver splits or role membership view names.
- The resulting router still rejects unexposed calls, incorrect argument types, incorrect receiver keys and missing authenticators.
- Exporting and restoring a `.liasse` artifact preserves enough information to construct the same router after restart.
- Bilani can delete its manifest-owned surface binding section instead of replacing it with a package parser.
- **This handoff file is deleted in the same commit that fixes the bug.**
