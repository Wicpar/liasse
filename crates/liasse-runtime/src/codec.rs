//! The built-in core codec namespaces (§16.1): `base64`, `hex`, and the
//! `string` byte-conversion functions.
//!
//! §16.1 lists `base64`, `hex`, and `string` among the standard *core*
//! namespaces — available without a `$requires` declaration, unlike an external
//! namespace (`liasse.cbor`, `liasse.cose`). The expression checker resolves the
//! Unicode `string` utilities (`string.lower`/`upper`/`trim`) natively, but the
//! byte codecs are not language builtins there. The migration transform path
//! (§20.1/§20.2) needs them — `base64.encode(string.bytes(.))` and its inverse —
//! so the runtime provides them as host namespaces resolved through the ordinary
//! [`HostNamespace`] machinery, seeded like the built-in `cose` contract.
//!
//! Each function is a deterministic pure map, so it is safe in a §16.3 `Pure`
//! position (a view, a check, a migration transform). `string.bytes` /
//! `string.from_bytes` sit under the `string` namespace beside the core Unicode
//! utilities: the checker resolves `string.trim` as a language builtin *before*
//! consulting a namespace, so the two coexist without collision.

use liasse_host::{
    ContractName, EffectClass, FunctionDescriptor, HostNamespace, InterfaceHash, InvocationFailure,
    NamespaceDescriptor, OpSignature, Version,
};
use liasse_value::{Bytes, Text, Type, Value};

/// The `(local namespace, "contract@major")` requirements that bind the built-in
/// codec namespaces (§16.1), for a [`HostBinding`](crate::host::HostBinding) that
/// serves the migration transform scope. Only the namespaces that actually build
/// are declared, so the two stay in lock-step.
pub(crate) fn requires() -> Vec<(String, String)> {
    definitions()
        .iter()
        .map(|def| (def.local.to_owned(), format!("{}@1", def.contract)))
        .collect()
}

/// The built-in codec namespaces to register (§16.1): `base64`, `hex`, and the
/// `string` byte codecs. A definition whose fixed contract name fails to parse
/// (impossible for these literals) is simply omitted, keeping this total.
pub(crate) fn namespaces() -> Vec<Box<dyn HostNamespace>> {
    definitions()
        .into_iter()
        .filter_map(|def| def.build().map(|ns| Box::new(ns) as Box<dyn HostNamespace>))
        .collect()
}

/// One codec namespace's static definition: its local key, contract name, and
/// the `(function, op, param, result)` functions it declares.
struct Definition {
    local: &'static str,
    contract: &'static str,
    functions: Vec<(&'static str, Op, Type, Type)>,
}

impl Definition {
    fn build(self) -> Option<CodecNamespace> {
        CodecNamespace::new(self.contract, self.functions)
    }
}

/// The fixed set of built-in codec namespaces (§16.1).
fn definitions() -> Vec<Definition> {
    vec![
        Definition {
            local: "base64",
            contract: "liasse.base64",
            functions: vec![
                ("encode", Op::Encode(Radix::Base64), Type::Bytes, Type::Text),
                ("decode", Op::Decode(Radix::Base64), Type::Text, Type::Bytes),
            ],
        },
        Definition {
            local: "hex",
            contract: "liasse.hex",
            functions: vec![
                ("encode", Op::Encode(Radix::Hex), Type::Bytes, Type::Text),
                ("decode", Op::Decode(Radix::Hex), Type::Text, Type::Bytes),
            ],
        },
        Definition {
            local: "string",
            contract: "liasse.string",
            functions: vec![
                ("bytes", Op::StringBytes, Type::Text, Type::Bytes),
                ("from_bytes", Op::StringFromBytes, Type::Bytes, Type::Text),
            ],
        },
    ]
}

/// One byte-codec operation.
#[derive(Clone, Copy)]
enum Op {
    /// `base64.encode(bytes) -> text` / `hex.encode(bytes) -> text`.
    Encode(Radix),
    /// `base64.decode(text) -> bytes` / `hex.decode(text) -> bytes`.
    Decode(Radix),
    /// `string.bytes(text) -> bytes` — the UTF-8 encoding of a text value.
    StringBytes,
    /// `string.from_bytes(bytes) -> text` — decode UTF-8 bytes to text.
    StringFromBytes,
}

/// The textual radix a byte codec renders to.
#[derive(Clone, Copy)]
enum Radix {
    Base64,
    Hex,
}

/// A built-in codec [`HostNamespace`] (§16.1): a fixed contract name and a small
/// set of deterministic pure byte-conversion functions.
struct CodecNamespace {
    descriptor: NamespaceDescriptor,
    ops: Vec<(&'static str, Op)>,
}

impl CodecNamespace {
    /// Assemble a namespace from `(name, op, param, result)` function specs, or
    /// `None` when the fixed contract name does not parse (dead for the literals
    /// in [`definitions`]). The interface hash is a fixed token: these are
    /// runtime-native contracts whose identity never drifts across a run.
    fn new(contract: &str, specs: Vec<(&'static str, Op, Type, Type)>) -> Option<Self> {
        let id = ContractName::parse(contract).ok()?;
        let functions = specs.iter().map(|(name, _, param, result)| {
            let signature = OpSignature::new([param.clone()], result.clone());
            ((*name).to_owned(), FunctionDescriptor::new(signature, EffectClass::Pure))
        });
        let descriptor = NamespaceDescriptor::new(
            id,
            Version::new(1, 0, 0),
            InterfaceHash::new(format!("builtin:{contract}@1")),
            std::iter::empty(),
            functions,
        );
        let ops = specs.into_iter().map(|(name, op, _, _)| (name, op)).collect();
        Some(Self { descriptor, ops })
    }

    fn op(&self, function: &str) -> Option<Op> {
        self.ops.iter().find(|(name, _)| *name == function).map(|(_, op)| *op)
    }
}

impl HostNamespace for CodecNamespace {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, args: &[Value]) -> Result<Value, InvocationFailure> {
        let op = self.op(function).ok_or_else(|| InvocationFailure::UnknownFunction(function.to_owned()))?;
        let [arg] = args else {
            return Err(InvocationFailure::Arity { function: function.to_owned(), expected: 1, found: args.len() });
        };
        match op {
            Op::Encode(radix) => Ok(Value::Text(Text::new(encode(radix, expect_bytes(function, arg)?)))),
            Op::Decode(radix) => Ok(Value::Bytes(decode(function, radix, expect_text(function, arg)?)?)),
            Op::StringBytes => Ok(Value::Bytes(Bytes::new(expect_text(function, arg)?.as_str().as_bytes().to_vec()))),
            Op::StringFromBytes => {
                let text = String::from_utf8(expect_bytes(function, arg)?.as_slice().to_vec())
                    .map_err(|_| verification(function, "bytes are not valid UTF-8"))?;
                Ok(Value::Text(Text::new(text)))
            }
        }
    }
}

fn encode(radix: Radix, bytes: &Bytes) -> String {
    match radix {
        Radix::Base64 => bytes.to_base64(),
        Radix::Hex => bytes.as_slice().iter().map(|b| format!("{b:02x}")).collect(),
    }
}

fn decode(function: &str, radix: Radix, text: &Text) -> Result<Bytes, InvocationFailure> {
    match radix {
        Radix::Base64 => {
            Bytes::from_base64(text.as_str()).map_err(|_| verification(function, "text is not canonical base64"))
        }
        Radix::Hex => decode_hex(text.as_str()).map_err(|reason| verification(function, reason)),
    }
}

/// Decode a lowercase/uppercase hex string to bytes (§16.1). An odd length or a
/// non-hex digit is rejected as invalid input.
fn decode_hex(text: &str) -> Result<Bytes, &'static str> {
    if !text.len().is_multiple_of(2) {
        return Err("hex text has an odd number of digits");
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let [hi, lo] = pair else { continue };
        out.push((hex_digit(*hi)? << 4) | hex_digit(*lo)?);
    }
    Ok(Bytes::new(out))
}

fn hex_digit(byte: u8) -> Result<u8, &'static str> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("hex text has a non-hex digit"),
    }
}

/// The `bytes` argument of a codec call. A wrong variant is impossible after the
/// checker types the call against the pinned signature, so it is reported as a
/// contract-honouring failure rather than trusted.
fn expect_bytes<'a>(function: &str, value: &'a Value) -> Result<&'a Bytes, InvocationFailure> {
    match value {
        Value::Bytes(bytes) => Ok(bytes),
        _ => Err(verification(function, "expected a bytes argument")),
    }
}

fn expect_text<'a>(function: &str, value: &'a Value) -> Result<&'a Text, InvocationFailure> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(verification(function, "expected a text argument")),
    }
}

fn verification(function: &str, detail: &str) -> InvocationFailure {
    InvocationFailure::Verification { detail: format!("`{function}`: {detail}") }
}
