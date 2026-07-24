//! The `module` value type and value (SPEC §13.16): a move-only, non-key,
//! non-decodable handle whose containers classify move-only by delegation.
//! Each expectation is derived from §8.5 (copy/move/affine) and §13.16, not from
//! prior implementation output.
//!
//! Written without `unwrap`/`expect`/`panic!`/indexing so the workspace deny-lints
//! hold here too: fallible construction threads through `Result` + `?`.

use liasse_value::{
    BlobDescriptor, Integer, MediaType, ModuleHandle, ModulePackageRef, ModuleType, Sha512,
    StructType, Type, Value,
};

fn any() -> Type {
    Type::Module(ModuleType::Any)
}

// --- move-only classification (the single hook + delegation) -------------------

#[test]
fn a_module_is_move_only() {
    // §8.5/§13.16: a module owns live state with one owner — it is never copyable.
    assert!(!any().is_copyable(), "a bare module is move-only");
    assert!(
        !Type::Module(ModuleType::Package(ModulePackageRef::new("t.acct", 1))).is_copyable(),
        "a package-refined module is move-only"
    );
    assert!(
        !Type::Module(ModuleType::Interface("credits".to_owned())).is_copyable(),
        "an interface-refined module is move-only"
    );
}

#[test]
fn a_container_of_a_module_is_move_only_by_delegation() {
    assert!(
        !Type::Set(Box::new(any())).is_copyable(),
        "{{ $set: module }}"
    );
    assert!(!Type::Optional(Box::new(any())).is_copyable(), "module?");
    assert!(
        !Type::View(Box::new(any())).is_copyable(),
        "{{ $view: module }}"
    );
    assert!(
        !Type::Map(Box::new(Type::Text), Box::new(any())).is_copyable(),
        "{{ $key: text, $value: module }}"
    );
    assert!(
        !Type::Struct(StructType::new([
            ("m".to_owned(), any()),
            ("n".to_owned(), Type::Int),
        ]))
        .is_copyable(),
        "a struct carrying a module field is move-only"
    );
    // Sanity: a struct with no module field stays copyable.
    assert!(
        Type::Struct(StructType::new([("n".to_owned(), Type::Int)])).is_copyable(),
        "a module-free struct is copyable"
    );
}

// --- name / key-eligibility ----------------------------------------------------

#[test]
fn a_module_is_named_and_not_key_eligible() {
    assert_eq!(any().name(), "module");
    assert!(
        !any().is_key_eligible(),
        "a module is not key-eligible (A.8)"
    );
    assert!(
        !Type::Struct(StructType::new([("m".to_owned(), any())])).is_key_eligible(),
        "a struct with a module field is not key-eligible"
    );
}

// --- decode rejection (no wire form) -------------------------------------------

#[test]
fn a_module_has_no_wire_decode_form() {
    // §13.16: a module is a live runtime handle produced only by selection/`unpack`,
    // never decoded from stored or wire data — so both boundaries reject it.
    let wire = serde_json::Value::String("anything".to_owned());
    assert!(
        any().decode(&wire).is_err(),
        "authoring decode rejects a module"
    );
    assert!(
        any().decode_wire(&wire).is_err(),
        "wire decode rejects a module"
    );
    assert!(
        any()
            .decode(&serde_json::json!({ "$module": { "name": "a", "space": "s" } }))
            .is_err(),
        "not even its own tagged debug form decodes back into a module"
    );
}

// --- value ordering / equality -------------------------------------------------

fn mounted(name: &str) -> Value {
    Value::Module(ModuleHandle::Mounted {
        space: "s".to_owned(),
        name: name.to_owned(),
    })
}

#[test]
fn module_handles_order_and_compare_coherently() -> Result<(), String> {
    let a = mounted("a");
    let a2 = mounted("a");
    let b = mounted("b");
    let sha = Sha512::parse(&"a".repeat(128)).map_err(|e| e.to_string())?;
    let pending = Value::Module(ModuleHandle::Pending(Box::new(BlobDescriptor::new(
        sha,
        42,
        MediaType::new("application/vnd.liasse+zip"),
        None,
    ))));
    let zero = Value::Int(Integer::parse("0").map_err(|e| e.to_string())?);

    assert_eq!(a, a2, "same (space, name) mounted handles are equal");
    assert_ne!(a, b, "different names are distinct");
    assert!(a < b, "mounted handles order by (space, name)");
    assert_eq!(pending.cmp(&pending), std::cmp::Ordering::Equal);
    assert_ne!(a, pending, "a mounted handle differs from a pending one");
    // A module ranks after every ordinary value and before `none` (the maximum).
    assert!(a > zero, "a module ranks after an int");
    assert!(a < Value::None, "`none` remains the maximum");
    Ok(())
}

// --- faithful (non-fake) audit wire form ---------------------------------------

#[test]
fn a_module_renders_a_faithful_module_tag() {
    // Not a data wire form — a faithful `$module` identity for audit/debug. The
    // guarantee it never round-trips as data is `decode` rejecting it (above).
    let text = mounted("a").to_canonical_json_string();
    assert!(text.contains("\"$module\""), "tagged under $module: {text}");
    assert!(
        text.contains("\"name\":\"a\""),
        "carries the instance name: {text}"
    );
    assert!(
        text.contains("\"space\":\"s\""),
        "carries the space: {text}"
    );
}
