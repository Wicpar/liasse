//! RED-TEAM finding — §13.1 / §6.2 / §9.1: a module `$model` field DEFAULT that
//! reads `$config` is BLIND to the installation configuration at genesis, so the
//! package-`$seed`/`$data` row it would fill is REJECTED and the install fails.
//!
//! §13.1: "`$config` declares an immutable typed struct for installation values;
//! defaults use the ordinary field rules, and module expressions read it through
//! `$config`." §6.2 lists `$config` among the structural bindings every module
//! expression carries, and §5.1 defines a default as "any value or view expression
//! visible from its declaration scope" — a field default is therefore a module
//! expression that MUST resolve `$config` to the configured value. §9.1 pins that
//! "Seed rows pass through the same defaults, normalization, checks, key, ref,
//! uniqueness, bucket, and meter rules as mutation inserts": a genesis seed
//! default resolves exactly as a mutation-insert default would on the installed
//! instance, where `$config` is bound. So a seeded row whose field default reads
//! `$config` MUST commit with the config-derived value.
//!
//! Instead the install is REJECTED at genesis seed admission. Root cause is an
//! ordering defect in `liasse-runtime`: `ModuleHost::install`
//! (`crates/liasse-runtime/src/modules/host.rs`) calls `Engine::load` (which
//! applies the package `$seed`/`$data` genesis and evaluates its defaults, ~L97)
//! BEFORE `engine.bind_config` (~L106). During that genesis `Engine::config` is
//! still `None`, and `Engine::base_context` (`engine.rs` ~L934) injects the
//! `$config` cell only when `config` is `Some`; the genesis seed default therefore
//! evaluates with no `$config` binding, cannot fill the field, and the row is
//! refused. The very next line's comment ("Done before the installation `$data`
//! overlay so an overlaid value may read `$config`") shows config is deliberately
//! bound before the installation-`$data` overlay but NOT before the package seed.
//!
//! The two FINDINGs below FAIL against the current implementation; the paired
//! CONTROLs (the same `$config` read in a computed value and in a `$view`, plus a
//! non-`$config` seed default) PASS, isolating the defect to reading `$config` in
//! a default resolved at genesis. Expectations are hand-derived from SPEC.md.
//!
//! A separate DRY positive (`dry_mutual_cross_collection_seed`) records that §9.1
//! cross-collection sibling seed visibility (mutual `count(/other)`) already holds.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
use std::collections::BTreeSet;
use std::path::Path;
use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<probe>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("probe"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation");
}

// ── FINDING 1 ────────────────────────────────────────────────────────────────
// §13.1/§6.2/§9.1: a seeded row whose field default reads `$config.currency` MUST
// commit at genesis with the configured value ("EUR"). The install is rejected.
#[test]
fn config_seed_default_read_rejects_genesis() {
    let text = r##"{
  format: 1
  name: config-seed-default-read-rejects-genesis
  suite: scenario
  spec: ["#modules", "§13.1", "#expressions", "§6.2", "#loading", "§9.1"]
  packages: {
    host: {
      $liasse: 1
      $app: "t.mod.host@1.0.0"
      $model: {
        companies: { $key: "id", id: "text", name: "text",
          modules: { $modules: { $interfaces: { templates: { $view: { $key: "id", id: "text", stored_ccy: "text" } } } } },
          catalog: { $view: ".modules::templates { module: modules.$key, id, stored_ccy, $sort: [module, id] }" }
        },
        $public: { catalog: { $params: { company: "text" }, $view: "/companies[@company].catalog" } }
      },
      $data: { companies: { acme: { name: "Acme" } } }
    }
    tplc: {
      $liasse: 1
      $module: "t.tplc@1.0.0"
      $config: { currency: "text = 'USD'" }
      $model: { templates: { $key: "id", id: "text", stored_ccy: "text = $config.currency" } }
      $data: { templates: { std: {} } }
      $expose: { templates: { $view: ".templates { id, stored_ccy }" } }
    }
  }
  root: host
  steps: [
    { module_install: { space: "/companies/acme/modules",
        request: { $name: "kit_eur", $module: "t.tplc@1.0.0", $config: { currency: "EUR" } } },
      expect: { outcome: ok } }
    { watch: "public.catalog", args: { company: "acme" }, id: "w1",
      expect_init: { value: [ { module: "kit_eur", id: "std", stored_ccy: "EUR" } ] } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "config-seed-default-read-rejects-genesis");
}

// ── FINDING 1b ───────────────────────────────────────────────────────────────
// The same defect with `$config` OMITTED at install: §13.3 says an omitted
// installation `$config` uses the package default ("USD"). The seed default reads
// that package-default `$config.currency` and MUST commit "USD". Still rejected —
// proving `$config` is simply UNBOUND during the genesis seed (not an
// override/merge ordering artifact of a supplied value).
#[test]
fn config_seed_default_read_rejects_with_default_config() {
    let text = r##"{
  format: 1
  name: config-seed-default-read-rejects-with-default-config
  suite: scenario
  spec: ["#modules", "§13.1", "§13.3", "#loading", "§9.1"]
  packages: {
    host: {
      $liasse: 1
      $app: "t.mod.host@1.0.0"
      $model: {
        companies: { $key: "id", id: "text", name: "text",
          modules: { $modules: { $interfaces: { templates: { $view: { $key: "id", id: "text", stored_ccy: "text" } } } } },
          catalog: { $view: ".modules::templates { module: modules.$key, id, stored_ccy, $sort: [module, id] }" }
        },
        $public: { catalog: { $params: { company: "text" }, $view: "/companies[@company].catalog" } }
      },
      $data: { companies: { acme: { name: "Acme" } } }
    }
    tplc: {
      $liasse: 1
      $module: "t.tplc@1.0.0"
      $config: { currency: "text = 'USD'" }
      $model: { templates: { $key: "id", id: "text", stored_ccy: "text = $config.currency" } }
      $data: { templates: { std: {} } }
      $expose: { templates: { $view: ".templates { id, stored_ccy }" } }
    }
  }
  root: host
  steps: [
    { module_install: { space: "/companies/acme/modules",
        request: { $name: "kit_def", $module: "t.tplc@1.0.0" } },
      expect: { outcome: ok } }
    { watch: "public.catalog", args: { company: "acme" }, id: "w1",
      expect_init: { value: [ { module: "kit_def", id: "std", stored_ccy: "USD" } ] } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "config-seed-default-read-rejects-with-default-config");
}

// ── CONTROL 1a ───────────────────────────────────────────────────────────────
// The identical `$config.currency` read in a `$view` resolves to the configured
// value at read time (install ok, direct_ccy = "EUR"). Isolates the defect to the
// default position — the `$config` binding IS present for post-install reads.
#[test]
fn control_config_read_in_view_resolves() {
    let text = r##"{
  format: 1
  name: control-config-read-in-view-resolves
  suite: scenario
  spec: ["#modules", "§13.1", "#views", "§7.1"]
  packages: {
    host: {
      $liasse: 1
      $app: "t.mod.host@1.0.0"
      $model: {
        companies: { $key: "id", id: "text", name: "text",
          modules: { $modules: { $interfaces: { templates: { $view: { $key: "id", id: "text", direct_ccy: "text" } } } } },
          catalog: { $view: ".modules::templates { module: modules.$key, id, direct_ccy, $sort: [module, id] }" }
        },
        $public: { catalog: { $params: { company: "text" }, $view: "/companies[@company].catalog" } }
      },
      $data: { companies: { acme: { name: "Acme" } } }
    }
    tplc: {
      $liasse: 1
      $module: "t.tplc@1.0.0"
      $config: { currency: "text = 'USD'" }
      $model: { templates: { $key: "id", id: "text" } }
      $data: { templates: { std: {} } }
      $expose: { templates: { $view: ".templates { id, direct_ccy: $config.currency }" } }
    }
  }
  root: host
  steps: [
    { module_install: { space: "/companies/acme/modules",
        request: { $name: "kit_eur", $module: "t.tplc@1.0.0", $config: { currency: "EUR" } } },
      expect: { outcome: ok } }
    { watch: "public.catalog", args: { company: "acme" }, id: "w1",
      expect_init: { value: [ { module: "kit_eur", id: "std", direct_ccy: "EUR" } ] } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "control-config-read-in-view-resolves");
}

// ── CONTROL 1b ───────────────────────────────────────────────────────────────
// The identical `$config.currency` read in a COMPUTED VALUE (§5.2) resolves to the
// configured value (install ok, shown_ccy = "EUR").
#[test]
fn control_config_read_in_computed_resolves() {
    let text = r##"{
  format: 1
  name: control-config-read-in-computed-resolves
  suite: scenario
  spec: ["#modules", "§13.1", "#state-model", "§5.2"]
  packages: {
    host: {
      $liasse: 1
      $app: "t.mod.host@1.0.0"
      $model: {
        companies: { $key: "id", id: "text", name: "text",
          modules: { $modules: { $interfaces: { templates: { $view: { $key: "id", id: "text", shown_ccy: "text" } } } } },
          catalog: { $view: ".modules::templates { module: modules.$key, id, shown_ccy, $sort: [module, id] }" }
        },
        $public: { catalog: { $params: { company: "text" }, $view: "/companies[@company].catalog" } }
      },
      $data: { companies: { acme: { name: "Acme" } } }
    }
    tplc: {
      $liasse: 1
      $module: "t.tplc@1.0.0"
      $config: { currency: "text = 'USD'" }
      $model: { templates: { $key: "id", id: "text", shown_ccy: "= $config.currency" } }
      $data: { templates: { std: {} } }
      $expose: { templates: { $view: ".templates { id, shown_ccy }" } }
    }
  }
  root: host
  steps: [
    { module_install: { space: "/companies/acme/modules",
        request: { $name: "kit_eur", $module: "t.tplc@1.0.0", $config: { currency: "EUR" } } },
      expect: { outcome: ok } }
    { watch: "public.catalog", args: { company: "acme" }, id: "w1",
      expect_init: { value: [ { module: "kit_eur", id: "std", shown_ccy: "EUR" } ] } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "control-config-read-in-computed-resolves");
}

// ── CONTROL 1c ───────────────────────────────────────────────────────────────
// A seed default that does NOT read `$config` (`= 'FIXED'`) commits at genesis and
// reads back — proving the seed-default machinery works; only the `$config` read
// inside it is blind. Pins the defect precisely to `$config`, not to defaults.
#[test]
fn control_noconfig_seed_default_commits() {
    let text = r##"{
  format: 1
  name: control-noconfig-seed-default-commits
  suite: scenario
  spec: ["#state-model", "§5.1", "#loading", "§9.1"]
  packages: {
    host: {
      $liasse: 1
      $app: "t.mod.host@1.0.0"
      $model: {
        companies: { $key: "id", id: "text", name: "text",
          modules: { $modules: { $interfaces: { templates: { $view: { $key: "id", id: "text", stored_ccy: "text" } } } } },
          catalog: { $view: ".modules::templates { module: modules.$key, id, stored_ccy, $sort: [module, id] }" }
        },
        $public: { catalog: { $params: { company: "text" }, $view: "/companies[@company].catalog" } }
      },
      $data: { companies: { acme: { name: "Acme" } } }
    }
    tplc: {
      $liasse: 1
      $module: "t.tplc@1.0.0"
      $config: { currency: "text = 'USD'" }
      $model: { templates: { $key: "id", id: "text", stored_ccy: "text = 'FIXED'" } }
      $data: { templates: { std: {} } }
      $expose: { templates: { $view: ".templates { id, stored_ccy }" } }
    }
  }
  root: host
  steps: [
    { module_install: { space: "/companies/acme/modules",
        request: { $name: "kit_eur", $module: "t.tplc@1.0.0", $config: { currency: "EUR" } } },
      expect: { outcome: ok } }
    { watch: "public.catalog", args: { company: "acme" }, id: "w1",
      expect_init: { value: [ { module: "kit_eur", id: "std", stored_ccy: "FIXED" } ] } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "control-noconfig-seed-default-commits");
}

// ── DRY POSITIVE ─────────────────────────────────────────────────────────────
// §9.1: within the single atomic seed load every seeded row identity is visible to
// every seed default across siblings — including CROSS-collection reads. Two
// collections whose defaults mutually count the other resolve to the full sibling
// counts: alpha (2 rows) each see count(/beta)=3; beta (3 rows) each see
// count(/alpha)=2. This HOLDS (no divergence) — recorded as a passing control.
#[test]
fn dry_mutual_cross_collection_seed() {
    let text = r##"{
  format: 1
  name: dry-mutual-cross-collection-seed
  suite: scenario
  spec: ["#loading", "§9.1"]
  package: {
    $liasse: 1
    $app: "t.mutseed@1.0.0"
    $model: {
      alpha: { $key: "id", id: "text", beta_seen: "int = count(/beta)" }
      beta:  { $key: "id", id: "text", alpha_seen: "int = count(/alpha)" }
      $public: {
        alpha: { $view: ".alpha { id, beta_seen }" }
        beta:  { $view: ".beta { id, alpha_seen }" }
      }
    }
    $data: {
      alpha: { a1: {}, a2: {} }
      beta:  { b1: {}, b2: {}, b3: {} }
    }
  }
  steps: [
    { watch: "public.alpha", id: "w1",
      expect_init: { value: { $unordered: [ { id: "a1", beta_seen: "3" }, { id: "a2", beta_seen: "3" } ] } } }
    { watch: "public.beta", id: "w2",
      expect_init: { value: { $unordered: [ { id: "b1", alpha_seen: "2" }, { id: "b2", alpha_seen: "2" }, { id: "b3", alpha_seen: "2" } ] } } }
  ]
}"##;
    assert_all_pass(&run_case_text(text), "dry-mutual-cross-collection-seed");
}
