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
use crate::header::{Header, Parsed};
use crate::mutation::{check_mutations, Mutation};
use crate::refs;
use crate::report::Reporter;
use crate::resolve::Resolver;
use crate::state::{Node, Shape};
use crate::surface::{check_surfaces, Surface};
use crate::{auth, blob, bucket, check, delete, meter, migration, seed};

/// A statically valid Liasse package model.
#[derive(Debug, Clone)]
pub struct Model {
    header: Header,
    root: Shape,
    types: BTreeMap<String, Node>,
    mutations: Vec<Mutation>,
    surfaces: Vec<Surface>,
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
        let mut diags = Diagnostics::new();
        let model = Self::assemble(sources, source, document, &mut diags);
        match model {
            Some(model) if !diags.has_errors() => Ok(model),
            _ => Err(diags),
        }
    }

    fn assemble(
        sources: &mut SourceMap,
        source: SourceId,
        document: &SpannedDocument,
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
        check::check_tree(&mut reporter, sources, &resolver, &root);
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
}
