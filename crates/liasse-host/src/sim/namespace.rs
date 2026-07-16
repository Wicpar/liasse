//! A scriptable host-namespace double covering the corpus `op` vocabulary
//! (tests/16-host-namespaces/NOTES.md and tests/23-host-contract/NOTES.md):
//! `double` (pure), `token` (generated), `accept` (verifier), plus the
//! nonconforming `off_type` (pure signature, returns a `text`) and `drifting`
//! (declared pure, returns a different value each evaluation).
//!
//! A generated/drifting result varies with the double's `phase`, which the
//! owner advances through [`SimNamespace::advance`] (`&mut`). Real per-
//! evaluation entropy is the runtime's to supply; the double stands in for it
//! deterministically so expectations stay externally deducible.

use std::collections::BTreeMap;

use liasse_value::num_bigint::BigInt;
use liasse_value::{Integer, Text, Value};

use crate::descriptor::{
    EffectClass, FunctionDescriptor, InterfaceHash, NamespaceDescriptor, NamespaceType, OpSignature,
};
use crate::namespace::{HostNamespace, InvocationFailure};
use crate::version::{ContractName, Version};

/// A deterministic behaviour a simulated function performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    /// Multiply the single integer argument by two (pure).
    Double,
    /// Return an opaque non-empty text, varying with `phase` (generated).
    Token,
    /// Verify the single text credential against the configured `accepts` map,
    /// returning the mapped proof or a verification failure (verifier).
    Accept,
    /// Return a `text` regardless of a pinned `(int) -> int` signature — a
    /// component returning an off-contract type (SPEC-ISSUES item 15).
    OffType,
    /// Declared `pure`, but return a different value each `phase` — a component
    /// lying about its effect class (SPEC-ISSUES items 15/16).
    Drifting,
}

/// One simulated function: its declared descriptor entry and its behaviour.
struct SimFunction {
    descriptor: FunctionDescriptor,
    behavior: Behavior,
}

/// A scriptable [`HostNamespace`] double.
pub struct SimNamespace {
    descriptor: NamespaceDescriptor,
    functions: BTreeMap<String, SimFunction>,
    accepts: BTreeMap<String, Value>,
    phase: u64,
}

impl SimNamespace {
    /// Start building a double for contract `id` at `version`.
    #[must_use]
    pub fn builder(
        id: ContractName,
        version: Version,
        interface_hash: InterfaceHash,
    ) -> SimNamespaceBuilder {
        SimNamespaceBuilder {
            id,
            version,
            interface_hash,
            functions: BTreeMap::new(),
            types: BTreeMap::new(),
            accepts: BTreeMap::new(),
        }
    }

    /// Advance the evaluation phase, so the next generated/drifting result
    /// differs from the last — the double's stand-in for a fresh evaluation or
    /// a replay.
    pub fn advance(&mut self) {
        self.phase = self.phase.wrapping_add(1);
    }

    fn run(&self, behavior: Behavior, args: &[Value]) -> Result<Value, InvocationFailure> {
        match behavior {
            Behavior::Double => self.double(args),
            Behavior::Token => Ok(Value::Text(Text::new(format!("tok-{}", self.phase)))),
            Behavior::Accept => self.accept(args),
            Behavior::OffType => Ok(Value::Text(Text::new("not-an-int"))),
            Behavior::Drifting => Ok(Value::Int(Integer::from(BigInt::from(self.phase)))),
        }
    }

    fn double(&self, args: &[Value]) -> Result<Value, InvocationFailure> {
        let [Value::Int(operand)] = args else {
            return Err(self.arity_or_type(args, "double"));
        };
        Ok(Value::Int(Integer::from(operand.as_bigint() * BigInt::from(2))))
    }

    fn accept(&self, args: &[Value]) -> Result<Value, InvocationFailure> {
        let [Value::Text(credential)] = args else {
            return Err(self.arity_or_type(args, "accept"));
        };
        self.accepts.get(credential.as_str()).cloned().ok_or_else(|| {
            InvocationFailure::Verification {
                detail: "credential is not accepted".to_owned(),
            }
        })
    }

    fn arity_or_type(&self, args: &[Value], function: &str) -> InvocationFailure {
        if args.len() != 1 {
            InvocationFailure::Arity {
                function: function.to_owned(),
                expected: 1,
                found: args.len(),
            }
        } else {
            InvocationFailure::Verification {
                detail: format!("`{function}` received an argument of the wrong type"),
            }
        }
    }
}

impl HostNamespace for SimNamespace {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, args: &[Value]) -> Result<Value, InvocationFailure> {
        let behavior = self
            .functions
            .get(function)
            .map(|f| f.behavior)
            .ok_or_else(|| InvocationFailure::UnknownFunction(function.to_owned()))?;
        self.run(behavior, args)
    }
}

/// Builder for a [`SimNamespace`].
pub struct SimNamespaceBuilder {
    id: ContractName,
    version: Version,
    interface_hash: InterfaceHash,
    functions: BTreeMap<String, SimFunction>,
    types: BTreeMap<String, NamespaceType>,
    accepts: BTreeMap<String, Value>,
}

impl SimNamespaceBuilder {
    /// Declare a function with its pinned signature, effect, and behaviour.
    #[must_use]
    pub fn function(
        mut self,
        name: impl Into<String>,
        signature: OpSignature,
        effect: EffectClass,
        behavior: Behavior,
    ) -> Self {
        self.functions.insert(
            name.into(),
            SimFunction {
                descriptor: FunctionDescriptor::new(signature, effect),
                behavior,
            },
        );
        self
    }

    /// Declare a namespace-defined named value type.
    #[must_use]
    pub fn named_type(mut self, name: impl Into<String>, ty: NamespaceType) -> Self {
        self.types.insert(name.into(), ty);
        self
    }

    /// Register a credential a `verifier` accepts, mapped to the proof it
    /// returns (§16 NOTES `accepts`).
    #[must_use]
    pub fn accepts(mut self, credential: impl Into<String>, proof: Value) -> Self {
        self.accepts.insert(credential.into(), proof);
        self
    }

    /// Finish the double.
    #[must_use]
    pub fn build(self) -> SimNamespace {
        let descriptor = NamespaceDescriptor::new(
            self.id,
            self.version,
            self.interface_hash,
            self.types,
            self.functions
                .iter()
                .map(|(name, f)| (name.clone(), f.descriptor.clone())),
        );
        SimNamespace {
            descriptor,
            functions: self.functions,
            accepts: self.accepts,
            phase: 0,
        }
    }
}
