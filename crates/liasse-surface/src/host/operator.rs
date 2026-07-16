//! Trusted host-operator transitions (SPEC.md §23.5).
//!
//! The Rust host MAY hold explicit operator capabilities for administration,
//! migration, recovery, or custom trusted operations. Operator access *bypasses
//! external role authentication* while retaining every other guarantee: the
//! package's type rules, refs, constraints, serial admission, and atomicity all
//! still bind, because an operator transition is admitted through the very same
//! engine pipeline a client `call` uses — only the §10/§11 surface authorization
//! is skipped.
//!
//! [`SurfaceHost::operator_call`] therefore resolves the target *mutation binding*
//! through the exposed router (so the receiver/parameter shape is known) but does
//! not verify an authenticator or evaluate role membership. A committed operator
//! transition drags every open subscription through the new head, exactly as a
//! client commit would (§12.6, §22.6).
//!
//! # Documented seams
//!
//! Host provenance for `$actor` under an operator transition is unpinned
//! (SPEC-ISSUES item 9), so this entry binds no synthetic actor. Addressing an
//! *internal* (unexposed) mutation — one with no surface declaration — needs the
//! internal-call runtime the surface layer does not yet reach, so an operator
//! target must currently name a surface-declared mutation.

use liasse_runtime::CallOutcome;
use liasse_store::InstanceStore;

use crate::address::SurfaceAddress;
use crate::binding::CallBinding;
use crate::outcome::{Denial, DenialReason, SurfaceOutcome};
use crate::request::SurfaceCall;
use crate::router::Resolved;

use super::{SurfaceError, SurfaceHost};

impl<S: InstanceStore> SurfaceHost<S> {
    /// Admit a trusted operator transition (§23.5): resolve the target mutation,
    /// bypass surface role authentication, and commit it through the ordinary
    /// admission pipeline. A committed transition sweeps every open subscription
    /// through the new head.
    ///
    /// The operation-identifier dedup (§12.3) and per-connection frontier settling
    /// of a client `call` do not apply — an operator transition is not scoped to a
    /// client connection — so any `operation_id` on `call` is ignored here.
    ///
    /// # Errors
    /// [`SurfaceError::Engine`] from a store or view fault during admission or the
    /// subscription sweep. A resolution or type refusal is an outcome, not an
    /// error.
    pub fn operator_call(&mut self, call: &SurfaceCall) -> Result<SurfaceOutcome, SurfaceError> {
        let binding = match self.operator_binding(call.address()) {
            Ok(binding) => binding,
            Err(denial) => return Ok(SurfaceOutcome::Denied(denial)),
        };
        // §11.1: a host-operator transition executes with no actor.
        let (request, _model) = match Self::build_request(&binding, call.args(), None) {
            Ok(pair) => pair,
            Err(rejection) => return Ok(SurfaceOutcome::Rejected(rejection)),
        };
        match self.engine.call(&request, &mut self.clock)? {
            CallOutcome::Committed { seq, response } => {
                self.sweep_all()?;
                Ok(SurfaceOutcome::Committed {
                    frontier: self.engine.head(),
                    commit: seq,
                    response,
                })
            }
            CallOutcome::Unchanged { response } => Ok(SurfaceOutcome::Unchanged {
                frontier: self.engine.head(),
                response,
            }),
            CallOutcome::Rejected(rejection) => Ok(SurfaceOutcome::Rejected(rejection)),
        }
    }

    /// Resolve an operator target to its mutation binding, ignoring authentication
    /// and role membership (§23.5). A view target is refused — an operator commits
    /// a transition, not a read.
    fn operator_binding(&self, address: &SurfaceAddress) -> Result<CallBinding, Denial> {
        match self.router.resolve(address)? {
            Resolved::PublicCall(binding) | Resolved::RoleCall { binding, .. } => Ok(binding.clone()),
            Resolved::PublicView(_) | Resolved::RoleView { .. } => Err(Denial::new(
                DenialReason::Unresolved,
                "an operator transition targets a mutation, not a view",
            )),
        }
    }
}
