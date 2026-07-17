//! The validated package model and its build orchestration.
//!
//! [`Model::build`] runs the phases in order — header, structural tree, ref
//! resolution, expression typing, mutations, surfaces, seed — into one shared
//! [`Diagnostics`], so a rejected package reports *every* static problem it can
//! find rather than only the first (SPEC.md multi-error requirement). A returned
//! [`Model`] is proof the definition passed every CORE static rule.

use std::collections::BTreeMap;

use liasse_diag::{Diagnostics, SourceId, SourceMap};
use liasse_syntax::SpannedDocument;

use crate::build::Builder;
use crate::doc::DocValueExt;
use crate::expose::ExposedInterface;
use crate::header::{Header, Parsed};
use crate::host::HostDescriptors;
use crate::mutation::{check_mutations, Mutation};
use crate::refs;
use crate::report::Reporter;
use crate::resolve::Resolver;
use crate::state::{Node, Shape};
use crate::surface::{check_surfaces, Surface};
use crate::{auth, blob, bucket, check, delete, expose, infer, meter, migration, module, seed};

/// A statically valid Liasse package model.
#[derive(Debug, Clone)]
pub struct Model {
    header: Header,
    root: Shape,
    types: BTreeMap<String, Node>,
    mutations: Vec<Mutation>,
    surfaces: Vec<Surface>,
    exposed: Vec<ExposedInterface>,
}

impl Model {
    /// Build and validate a package from its parsed definition document.
    ///
    /// `source` is the [`SourceId`] under which `document`'s text is registered
    /// in `sources`; expression sub-sources are registered as the build checks
    /// them, so `sources` must be the same map the caller renders diagnostics
    /// against. On any static rejection this returns the accumulated
    /// [`Diagnostics`]; otherwise the proof-carrying [`Model`].
    pub fn build(
        sources: &mut SourceMap,
        source: SourceId,
        document: &SpannedDocument,
    ) -> Result<Self, Diagnostics> {
        Self::build_with_hosts(sources, source, document, &HostDescriptors::default())
    }

    /// Build and validate a package whose expressions may call the resolved
    /// `$requires` host namespaces `hosts` describes (§16.2).
    ///
    /// A `$view`/`$default`/computed/`$check`/`$normalize` host-namespace call
    /// type-checks against its pinned signature and the position's effect policy
    /// (§16.3) rather than faulting as an unknown function. The runtime supplies
    /// these after resolving `$requires` against its host registry; [`Model::build`]
    /// passes none, so a package with no host requirements is unaffected. Other
    /// arguments and the failure discipline match [`Model::build`].
    pub fn build_with_hosts(
        sources: &mut SourceMap,
        source: SourceId,
        document: &SpannedDocument,
        hosts: &HostDescriptors,
    ) -> Result<Self, Diagnostics> {
        let mut diags = Diagnostics::new();
        let model = Self::assemble(sources, source, document, hosts, &mut diags);
        match model {
            Some(model) if !diags.has_errors() => Ok(model),
            _ => Err(diags),
        }
    }

    fn assemble(
        sources: &mut SourceMap,
        source: SourceId,
        document: &SpannedDocument,
        hosts: &HostDescriptors,
        diags: &mut Diagnostics,
    ) -> Option<Self> {
        let mut reporter = Reporter::new(source, diags);
        let Parsed {
            header,
            model,
            types,
            data,
        } = Header::build(&mut reporter, document.root())?;
        let model = model?;

        let build = Builder::run(&mut reporter, model, types);
        let mut root = build.root;
        refs::resolve(&mut reporter, &mut root);

        let resolver = Resolver::new(&build.types);
        // §5.1/§5.2: refine each model-root computed value's placeholder `json`
        // type from its expression before the tree check, so a reference `.name`
        // resolves to the value's real type (a `bool` condition, an `int` operand)
        // rather than the widest `json`. Diagnostics are the tree check's job.
        infer::root_computed_types(sources, &resolver, &mut root);
        // §14.4–§14.6: type each source-backed bucket into its temporal-collection
        // row before the tree/surface checks, so a temporal selector over the
        // bucket resolves against real output-field and structural-binding types.
        bucket::type_source_buckets(&mut reporter, sources, &resolver, &mut root, &build.source_bucket_decls);
        // §13.8/§13.9: type each `$modules` space into its instance-keyed view of
        // declared interface contracts before the tree/surface checks, so
        // `.modules::iface` aggregation and `modules.$key` resolve.
        module::type_module_spaces(&resolver, &mut root, &build.module_spaces);
        check::check_tree(&mut reporter, sources, &resolver, &root, hosts);
        let mutations = check_mutations(
            &mut reporter,
            sources,
            &resolver,
            &root,
            &build.raw_muts,
            &build.source_buckets,
        );
        let surfaces = check_surfaces(
            &mut reporter,
            sources,
            &resolver,
            &root,
            &mutations,
            &build.surfaces,
        );
        auth::check(&mut reporter, sources, &build.auths, &build.surfaces);
        bucket::check(&mut reporter, sources, &resolver, &root, &build.buckets);
        meter::check(&mut reporter, sources, &build.limits, &build.consumes);
        blob::check_all(&mut reporter, &build.blob_storage);
        delete::check(&mut reporter, sources, &root, &build.raw_muts);
        if let Some(migrations) = document.root().member("$migrations") {
            migration::check(&mut reporter, sources, &migrations.value);
        }
        // §13.8: type each `$expose` interface `$view` against the module root and
        // validate its `$mut` bindings, capturing the interfaces the runtime
        // evaluates against a child instance and resolves an interface-addressed
        // call through.
        let exposed = expose::check_and_capture(
            &mut reporter,
            sources,
            &resolver,
            &root,
            &mutations,
            document.root().member("$expose").map(|m| &m.value),
        );
        if let Some(data) = data {
            seed::check_seed(&mut reporter, &root, data);
        }
        // `resolver` borrows `build.types`; its last use above lets NLL release
        // the borrow so the table can move into the model below.

        Some(Self {
            header,
            root,
            types: build.types,
            mutations,
            surfaces,
            exposed,
        })
    }

    /// The validated package header (identity, kind).
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The root state shape (`$model`).
    #[must_use]
    pub fn root(&self) -> &Shape {
        &self.root
    }

    /// The reusable `$types` shapes, by name.
    #[must_use]
    pub fn types(&self) -> &BTreeMap<String, Node> {
        &self.types
    }

    /// Every validated mutation, flat, with its receiver path.
    #[must_use]
    pub fn mutations(&self) -> &[Mutation] {
        &self.mutations
    }

    /// Every validated surface (public and role-granted).
    #[must_use]
    pub fn surfaces(&self) -> &[Surface] {
        &self.surfaces
    }

    /// The module interfaces this package exposes (§13.8): each a child-visible
    /// handle bound to a private `$view` projection and callable mutations. Empty
    /// for a package with no top-level `$expose`. The composition runtime
    /// evaluates the `$view` against a child instance to serve an interface read
    /// and resolves an interface-addressed call through the bound mutations.
    #[must_use]
    pub fn exposed_interfaces(&self) -> &[ExposedInterface] {
        &self.exposed
    }
}
