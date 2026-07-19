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

use liasse_expr::ExprType;

use crate::build::Builder;
use crate::config::ConfigSchema;
use crate::doc::DocValueExt;
use crate::expose::ExposedInterface;
use crate::header::{Header, Parsed};
use crate::host::HostDescriptors;
use crate::migration::Migrations;
use crate::mutation::{check_mutations, Mutation};
use crate::refs;
use crate::report::Reporter;
use crate::resolve::Resolver;
use crate::state::{ExprSource, Node, Shape};
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
    config: Option<ConfigSchema>,
    migrations: Migrations,
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

        let build = Builder::run(&mut reporter, model, types, document.root().member("$config").map(|m| &m.value));
        let mut root = build.root;
        refs::resolve(&mut reporter, &mut root);

        let resolver = Resolver::new(&build.types);
        // §13.1: a module's `$config` resolves to a keyless struct row. A module's
        // authored expressions read it through `$config`, so the row is bound as a
        // structural in every expression phase below; the runtime type-checks
        // install values against the same schema retained on the model. `None` for
        // an application or a module with no `$config`, which binds nothing.
        let config_row = build.config.as_ref().map(|shape| resolver.shape_row(shape));
        let config_binding = config_row.as_ref().map(|row| ExprType::Row(row.clone()));
        // §5.1/§5.2/§5.3: refine each computed value's placeholder `json` type
        // from its expression before the tree check — at the model root and in
        // every nested struct and collection — so a reference `.name` resolves to
        // the value's real type (a `bool` condition, an `int` operand) rather than
        // the widest `json`, which has no typed operator. Diagnostics are the tree
        // check's job.
        infer::computed_types(sources, &resolver, &mut root, config_binding.as_ref());
        // §14.4–§14.6: type each source-backed bucket into its temporal-collection
        // row before the tree/surface checks, so a temporal selector over the
        // bucket resolves against real output-field and structural-binding types.
        bucket::type_source_buckets(&mut reporter, sources, &resolver, &mut root, &build.source_bucket_decls);
        // §13.8/§13.9: type each `$modules` space into its instance-keyed view of
        // declared interface contracts before the tree/surface checks, so
        // `.modules::iface` aggregation and `modules.$key` resolve.
        module::type_module_spaces(&resolver, &mut root, &build.module_spaces);
        check::check_tree(&mut reporter, sources, &resolver, &root, hosts, config_binding.as_ref());
        let mutations = check_mutations(
            &mut reporter,
            sources,
            &resolver,
            &root,
            &build.raw_muts,
            &build.source_buckets,
            config_binding.as_ref(),
        );
        let surfaces = check_surfaces(
            &mut reporter,
            sources,
            &resolver,
            &root,
            &mutations,
            &build.surfaces,
            config_binding.as_ref(),
        );
        auth::check(&mut reporter, sources, &build.auths, &build.surfaces);
        bucket::check(&mut reporter, sources, &resolver, &root, &build.buckets);
        meter::check(&mut reporter, sources, &build.limits, &build.consumes);
        blob::check_all(&mut reporter, &build.blob_storage);
        delete::check(&mut reporter, sources, &root, &build.raw_muts);
        // §20.1/§4: `$migrations` is an optional model-root declaration (a sibling
        // of the collections inside `$model`); the §4 authoring form also admits it
        // as a top-level member. Validate its shape wherever it is declared and
        // retain the parsed programs so the runtime can compile them (§20.1).
        let migrations = model
            .member("$migrations")
            .or_else(|| document.root().member("$migrations"))
            .map(|m| migration::check(&mut reporter, sources, &m.value))
            .unwrap_or_default();
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
            config_binding.as_ref(),
        );
        if let Some(data) = data {
            seed::check_seed(&mut reporter, &root, data);
        }
        // §13.1: retain the resolved `$config` struct row and its per-member
        // defaults as the schema the runtime type-checks install values against.
        let config = config_row.map(|row| ConfigSchema::new(row, config_defaults(build.config.as_ref())));
        // `resolver` borrows `build.types`; its last use above lets NLL release
        // the borrow so the table can move into the model below.

        Some(Self {
            header,
            root,
            types: build.types,
            mutations,
            surfaces,
            exposed,
            config,
            migrations,
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

    /// The declared `$config` struct schema (§13.1), for a module package that
    /// declares one; `None` for an application or a module with no `$config`.
    ///
    /// Analogous to [`exposed_interfaces`](Self::exposed_interfaces): the
    /// composition runtime consumes it to type-check an installation's supplied
    /// `$config` values against the declared member types — rejecting an unknown
    /// member or a type mismatch (§13.3) — and to bind `$config` to the schema's
    /// [`row_type`](ConfigSchema::row_type) as the structural value a child's
    /// expressions read (§13.1). The model has already bound `$config` in this
    /// package's own authored expressions against the same schema.
    #[must_use]
    pub fn config_schema(&self) -> Option<&ConfigSchema> {
        self.config.as_ref()
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

    /// The retained `$migrations` programs (§20.1): the ordered statement texts of
    /// each exact source-version migration program. The runtime compiles and runs
    /// the program whose key matches the active source package version when a
    /// package update targets this model.
    #[must_use]
    pub fn migrations(&self) -> &Migrations {
        &self.migrations
    }
}

/// Each `$config` member's default expression, by member name (§13.1). A member
/// with a default MAY be omitted by an installation; one without is required.
fn config_defaults(config: Option<&Shape>) -> BTreeMap<String, ExprSource> {
    let Some(shape) = config else {
        return BTreeMap::new();
    };
    shape
        .members
        .iter()
        .filter_map(|member| match &member.node {
            Node::Scalar(field) => field
                .default
                .clone()
                .map(|default| (member.name.as_str().to_owned(), default)),
            _ => None,
        })
        .collect()
}
