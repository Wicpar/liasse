//! Source-backed and recurring bucket collections (SPEC.md §14.4–§14.6).
//!
//! A source-backed bucket derives its rows from a `$source` view rather than from
//! stored state: each source row evaluates `$from`, `$until`, and (for a recurring
//! bucket) `$repeat` to a period, and produces one interval row per generated
//! period. The derived rows expose the source identity and interval bounds as the
//! structural bindings `$source`/`$from`/`$until`/`$index` (§14.4), and carry the
//! collection's own output fields (`plan: "= $source.plan"`), all read-only.
//!
//! The model projects such a collection as a placeholder view whose row type it
//! computes; the runtime keeps neither the typed expressions nor a materializer,
//! so — like buckets and meters — this module reads the declaration from the
//! definition document, compiles the `$source`/`$from`/`$until`/`$repeat`/output
//! expressions once, and materializes the interval rows on demand
//! ([`CompiledSourceBucket::materialize`]).
//!
//! Period-to-timestamp arithmetic and the interval series itself are
//! [`liasse_value`] operations (`Period::advance`, `recurring_intervals`); this
//! module only binds the structural context each expression reads and assembles
//! the resulting rows.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_expr::{Cell, ExprType, Row, RowId, RowType, TypedExpr};
use liasse_value::{recurring_intervals, Integer, Interval, Period, Struct, Text, Timestamp, Value};

use crate::compiled::compile_expr;
use crate::env::{NamedExtant, RuntimeEnv};
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::keyring_view::KeyringSnapshot;
use crate::schema::Schema;
use crate::scope::RuntimeScope;

/// A compiled source-backed bucket collection (§14.4–§14.6).
pub(crate) struct CompiledSourceBucket {
    /// The top-level collection name the derived rows live under.
    pub(crate) name: String,
    /// The `$source` view, typed over the parent (package root) scope.
    source: TypedExpr,
    /// `$from` — the interval start, typed over the source scope.
    from: TypedExpr,
    /// `$until` — the optional series upper bound (absent ⇒ unbounded).
    until: Option<TypedExpr>,
    /// `$repeat` — the optional recurrence period (absent ⇒ one interval).
    repeat: Option<TypedExpr>,
    /// The collection's output fields, each typed over the source scope.
    outputs: Vec<(String, TypedExpr)>,
    /// A custom `$key` (§14.6): its component expressions over the source scope.
    /// `None` selects the inferred identity (source identity, plus `$from` when the
    /// bucket recurs).
    key: Option<Vec<TypedExpr>>,
    /// Whether the bucket recurs (`$repeat` present): a recurring bucket's inferred
    /// identity extends the source identity with `$from` (§14.6).
    repeating: bool,
}

/// The structural cell names a derived bucket row carries (§14.4).
const SOURCE_CELL: &str = "$source";
const FROM_CELL: &str = "$from";
const UNTIL_CELL: &str = "$until";
const INDEX_CELL: &str = "$index";

/// Compile every source-backed bucket declaration in the model (§14.4). A
/// source-backed bucket is a root member the model projected as a view whose
/// document shape carries a `$bucket` object with a `$source`.
pub(crate) fn compile(
    sources: &mut SourceMap,
    schema: Schema<'_>,
    root_ty: &ExprType,
    model_doc: &liasse_syntax::DocValue,
) -> Result<Vec<CompiledSourceBucket>, EngineError> {
    let mut out = Vec::new();
    for member in &schema.model().root().members {
        if !matches!(&member.node, liasse_model::Node::View(_)) {
            continue;
        }
        let name = member.name.as_str().to_owned();
        let Some(shape) = crate::doc::shape_at(model_doc, std::slice::from_ref(&name)) else {
            continue;
        };
        let Some(bucket_doc) = crate::doc::member(shape, "$bucket").and_then(crate::doc::object)
        else {
            continue;
        };
        if !bucket_doc.iter().any(|m| m.name.text == "$source") {
            continue;
        }
        if let Some(compiled) = compile_one(sources, root_ty, &name, shape, bucket_doc)? {
            out.push(compiled);
        }
    }
    Ok(out)
}

/// Whether the root member `name` is a source-backed bucket in `model_doc` — a
/// view whose shape carries a `$bucket` object with a `$source`. Lets
/// [`compile_views`](crate::compiled) skip it (its rows are materialized here, not
/// evaluated from its placeholder `.` expression).
pub(crate) fn is_source_bucket(model_doc: &liasse_syntax::DocValue, name: &str) -> bool {
    crate::doc::shape_at(model_doc, std::slice::from_ref(&name.to_owned()))
        .and_then(|shape| crate::doc::member(shape, "$bucket"))
        .and_then(crate::doc::object)
        .is_some_and(|members| members.iter().any(|m| m.name.text == "$source"))
}

fn compile_one(
    sources: &mut SourceMap,
    root_ty: &ExprType,
    name: &str,
    shape: &liasse_syntax::DocValue,
    bucket_doc: &[liasse_syntax::DocMember],
) -> Result<Option<CompiledSourceBucket>, EngineError> {
    let source_text = bucket_doc
        .iter()
        .find(|m| m.name.text == "$source")
        .and_then(|m| crate::doc::string(&m.value));
    let Some(source_text) = source_text else { return Ok(None) };

    // The `$source` view is read against the package root (top-level buckets only
    // in CORE scope), so its `.` is the root.
    let source_scope = RuntimeScope::new(root_ty.clone(), root_ty.clone());
    let (source, _) = compile_expr(sources, &source_scope, "bucket-source", source_text)?;
    let Some(source_row) = source.ty().as_view().or_else(|| source.ty().as_row()).cloned() else {
        return Ok(None);
    };

    // Every other bucket/output expression reads the derived row's structural
    // bindings, not a stored `.`; a keyless `.` keeps the scope well-formed (§14.4).
    let ts = ExprType::scalar(liasse_value::Type::timestamp());
    let opt_ts = ExprType::scalar(liasse_value::Type::Optional(Box::new(liasse_value::Type::timestamp())));
    let scope = RuntimeScope::new(
        ExprType::Row(RowType::keyless(std::iter::empty::<(String, ExprType)>())),
        root_ty.clone(),
    )
    .with_structural("source", ExprType::Row(source_row))
    .with_structural("from", ts)
    .with_structural("until", opt_ts)
    .with_structural("index", ExprType::scalar(liasse_value::Type::Int));

    let mut from = None;
    let mut until = None;
    let mut repeat = None;
    for member in bucket_doc {
        let Some(text) = crate::doc::string(&member.value) else { continue };
        match member.name.text.as_str() {
            "$source" => {}
            "$from" => from = Some(compile_expr(sources, &scope, "bucket-from", text)?.0),
            "$until" => until = Some(compile_expr(sources, &scope, "bucket-until", text)?.0),
            "$repeat" => repeat = Some(compile_expr(sources, &scope, "bucket-repeat", text)?.0),
            _ => {}
        }
    }
    let Some(from) = from else { return Ok(None) };

    let mut outputs = Vec::new();
    let mut key = None;
    for member in crate::doc::object(shape).into_iter().flatten() {
        let field = member.name.text.as_str();
        if field == "$key" {
            key = Some(compile_key(sources, &scope, &member.value)?);
            continue;
        }
        if field.starts_with('$') {
            continue;
        }
        let Some(raw) = crate::doc::string(&member.value) else { continue };
        let expr = raw.trim_start().strip_prefix('=').map_or(raw, str::trim);
        let (typed, _) = compile_expr(sources, &scope, "bucket-output", expr)?;
        outputs.push((field.to_owned(), typed));
    }

    Ok(Some(CompiledSourceBucket {
        name: name.to_owned(),
        source,
        from,
        until,
        repeat: repeat.clone(),
        outputs,
        key,
        repeating: repeat.is_some(),
    }))
}

/// Compile a custom `$key` (§14.6): a single component expression or an array of
/// them, each read in the source scope.
fn compile_key(
    sources: &mut SourceMap,
    scope: &RuntimeScope,
    value: &liasse_syntax::DocValue,
) -> Result<Vec<TypedExpr>, EngineError> {
    let mut components = Vec::new();
    if let Some(text) = crate::doc::string(value) {
        components.push(compile_expr(sources, scope, "bucket-key", text)?.0);
    } else if let Some(items) = crate::doc::array(value) {
        for item in items {
            if let Some(text) = crate::doc::string(item) {
                components.push(compile_expr(sources, scope, "bucket-key", text)?.0);
            }
        }
    }
    Ok(components)
}

/// The inputs a source-bucket materialization reads: the base package root (its
/// stored collections, without derived buckets, to avoid recursion) plus the
/// generative and out-of-band context.
pub(crate) struct BucketInputs<'a> {
    pub(crate) base_root: &'a Row,
    pub(crate) params: &'a BTreeMap<String, Cell>,
    pub(crate) context: &'a BTreeMap<String, Cell>,
    pub(crate) now: Timestamp,
    pub(crate) seed: u64,
    pub(crate) keyrings: &'a [KeyringSnapshot],
}

impl CompiledSourceBucket {
    /// Materialize the derived interval rows of this bucket (§14.4–§14.5).
    ///
    /// `horizon` bounds the generation of an unbounded recurring series (the
    /// read/evaluation instant). When `filter_active` holds, only rows active at
    /// `inputs.now` are kept — the pool rows a bare bucketed read or a spend-time
    /// meter source observes (§14.1, §15.1); otherwise every generated row is kept
    /// (the full extant set a temporal selector re-derives activity over, §14.2).
    ///
    /// A source row whose series does not evaluate (a non-advancing period, an
    /// invalid series bound, or an unevaluable bound) contributes no rows — the
    /// admission-time [`validate`] pass rejects such a transition before it commits,
    /// so committed state never reaches this path in that state.
    pub(crate) fn materialize(
        &self,
        inputs: &BucketInputs<'_>,
        horizon: Timestamp,
        filter_active: bool,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for source_row in self.source_rows(inputs) {
            self.rows_for_source(inputs, &source_row, horizon, filter_active, &mut rows);
        }
        rows
    }

    /// Evaluate the `$source` view against the base root, yielding the source rows.
    fn source_rows(&self, inputs: &BucketInputs<'_>) -> Vec<Row> {
        let env = self.env(inputs, BTreeMap::new());
        let current = Cell::Row(Box::new(inputs.base_root.clone()));
        match self.source.evaluate(&env, &current) {
            Ok(Cell::Collection(rows)) => rows,
            Ok(Cell::Row(row)) => vec![*row],
            _ => Vec::new(),
        }
    }

    /// Generate the interval rows of one source row.
    fn rows_for_source(
        &self,
        inputs: &BucketInputs<'_>,
        source_row: &Row,
        horizon: Timestamp,
        filter_active: bool,
        out: &mut Vec<Row>,
    ) {
        let Some((from, until, repeat)) = self.bounds(inputs, source_row) else { return };
        let Ok(intervals) = recurring_intervals(from, until, repeat.as_ref(), horizon) else {
            return;
        };
        for interval in intervals {
            if filter_active && !active_at(interval.from, interval.until, inputs.now) {
                continue;
            }
            out.push(self.build_row(inputs, source_row, interval));
        }
    }

    /// Evaluate `$from`, `$until`, and `$repeat` for one source row.
    fn bounds(
        &self,
        inputs: &BucketInputs<'_>,
        source_row: &Row,
    ) -> Option<(Timestamp, Option<Timestamp>, Option<Period>)> {
        let structurals =
            BTreeMap::from([(SOURCE_CELL[1..].to_owned(), Cell::Row(Box::new(source_row.clone())))]);
        let env = self.env(inputs, structurals);
        let current = keyless_current();
        let from = match self.from.evaluate(&env, &current).ok()? {
            Cell::Scalar(Value::Timestamp(ts)) => ts,
            _ => return None,
        };
        let until = match &self.until {
            Some(expr) => match expr.evaluate(&env, &current).ok()? {
                Cell::Scalar(Value::Timestamp(ts)) => Some(ts),
                _ => None,
            },
            None => None,
        };
        let repeat = match &self.repeat {
            Some(expr) => match expr.evaluate(&env, &current).ok()? {
                Cell::Scalar(Value::Period(period)) => Some(*period),
                _ => None,
            },
            None => None,
        };
        Some((from, until, repeat))
    }

    /// Build one derived interval row: the output fields plus the
    /// `$source`/`$from`/`$until`/`$index` structural cells (§14.4).
    fn build_row(&self, inputs: &BucketInputs<'_>, source_row: &Row, interval: liasse_value::Interval) -> Row {
        let from_cell = Cell::Scalar(Value::Timestamp(interval.from));
        let until_cell = Cell::Scalar(interval.until.map_or(Value::None, Value::Timestamp));
        let index_cell = Cell::Scalar(Value::Int(Integer::from(interval.index)));
        let source_cell = Cell::Row(Box::new(source_row.clone()));

        let structurals = BTreeMap::from([
            (SOURCE_CELL[1..].to_owned(), source_cell.clone()),
            (FROM_CELL[1..].to_owned(), from_cell.clone()),
            (UNTIL_CELL[1..].to_owned(), until_cell.clone()),
            (INDEX_CELL[1..].to_owned(), index_cell.clone()),
        ]);
        let env = self.env(inputs, structurals);
        let current = keyless_current();

        let mut cells: Vec<(String, Cell)> = Vec::new();
        for (name, expr) in &self.outputs {
            let cell = expr.evaluate(&env, &current).unwrap_or(Cell::Scalar(Value::None));
            cells.push((name.clone(), cell));
        }
        cells.push((SOURCE_CELL.to_owned(), source_cell));
        cells.push((FROM_CELL.to_owned(), from_cell));
        cells.push((UNTIL_CELL.to_owned(), until_cell));
        cells.push((INDEX_CELL.to_owned(), index_cell));

        let (id, key) = self.identity(&env, &current, source_row, interval.from);
        Row::new(id, key, cells)
    }

    /// The identity and key of a derived row (§14.6). A custom `$key` builds both
    /// from its component expressions; the inferred identity is the source identity,
    /// extended with `$from` (and its start value) when the bucket recurs.
    fn identity(
        &self,
        env: &RuntimeEnv<'_>,
        current: &Cell,
        source_row: &Row,
        from: Timestamp,
    ) -> (RowId, Value) {
        if let Some(components) = &self.key {
            let values: Vec<Value> = components
                .iter()
                .map(|expr| match expr.evaluate(env, current) {
                    Ok(Cell::Scalar(value)) => value,
                    _ => Value::None,
                })
                .collect();
            let key = composite_value(&values);
            let id = RowId::keyed(key_text(&values));
            return (id, key);
        }
        let start_text = from.to_canonical_text();
        if self.repeating {
            let key = Value::Struct(Struct::new([
                (Text::new("source"), source_row.key().clone()),
                (Text::new("from"), Value::Timestamp(from)),
            ]));
            (source_row.id().child_keyed(start_text), key)
        } else {
            (source_row.id().clone(), source_row.key().clone())
        }
    }

    /// A [`RuntimeEnv`] over the base root carrying the given `structurals` merged
    /// with the request context (`$actor`/`$session`). The temporal index is empty:
    /// a source view reads stored collections, never a bucketed base, in CORE scope.
    fn env(&self, inputs: &BucketInputs<'_>, structurals: BTreeMap<String, Cell>) -> RuntimeEnv<'static> {
        let mut merged = inputs.context.clone();
        merged.extend(structurals);
        RuntimeEnv::new(
            inputs.base_root.clone(),
            inputs.params.clone(),
            BTreeMap::new(),
            merged,
            inputs.now,
            inputs.seed,
            // A source-backed bucket row is derived, not admitted, so its key never
            // draws a fresh generated value; it evaluates at the root generation.
            crate::generator::Generation::ROOT,
            Vec::new(),
            // A source view / derived-row key reads stored collections directly and
            // evaluates no temporal selector, so it needs no source-bucket horizon.
            None,
            inputs.keyrings.to_vec(),
            // A source view / derived-row key reads stored collections, never a
            // blob placement member (§14.4–§14.6), so it carries no placement facts.
            crate::env::BlobPlacements::default(),
            // A source view / derived-row key is a pure expression (§14.4–§14.6);
            // it resolves no host call, so it needs no live dispatch.
            crate::host::HostDispatch::none(inputs.now),
        )
    }

    /// Validate every source row's series (§14.5, §14.7) for admission, eagerly at
    /// the source transition: a finite series bound MUST sit above its start, every
    /// recurrence step MUST advance strictly, and (for a calendar `$repeat`) no
    /// boundary of the enumerable series may land on an `overflow: reject` date
    /// missing from its destination month (§14.7). [`recurring_intervals`] surfaces
    /// each of these as an error over the enumerable series — a finite series is
    /// generated in full here regardless of the minimal horizon, so an overflow at a
    /// later boundary is caught at admission, not deferred to a temporal read.
    ///
    /// A custom `$key` additionally MUST be unique for every generated row (§14.6):
    /// two generated rows sharing a custom key would collapse to one identity,
    /// corrupting §12.4 deltas and the §15 meter pool index. This is a runtime
    /// property of the derived series (two source rows, or two periods of one source,
    /// may resolve the same key), so it is enforced here at admission — the model
    /// records only that the invariant exists ([`check_custom_key`]), never the data.
    /// A finite series is enumerated in full, so its uniqueness (intra- and
    /// cross-source) is proven directly. An unbounded recurring series is infinite
    /// and cannot be enumerated, so its cross-source uniqueness is instead proven
    /// structurally and GRID-AWARE: two sources collide only when they resolve the
    /// same key under equal `$source`/`$index`/`$until` (they group under one
    /// [`Self::probe_identity`]) AND their `$from` recurrence grids can actually
    /// share a boundary. For two fixed-period grids `{φ + k·P}` a shared boundary
    /// exists iff `(φ1 − φ2)` is a multiple of `gcd(P1, P2)`; a phase offset that is
    /// not makes the grids DISJOINT, so a `$from`-bearing key stays unique and must
    /// load ([`GridPhase::provably_disjoint`]). The check is sound-but-conservative:
    /// it accepts only when it can PROVE no collision (disjoint fixed-period grids
    /// with a `$from`-identity key component) and otherwise rejects, so a
    /// calendar-period grid, a key whose `$from` dependence is not a direct
    /// component, or a genuinely aligned grid all stay rejected. The wave-3 aligned
    /// collision (offset a multiple of the period) therefore still rejects, while a
    /// phase-offset-disjoint grid now loads.
    ///
    /// Rejects the whole transition on the first offending source row.
    pub(crate) fn validate(&self, inputs: &BucketInputs<'_>) -> Result<(), Rejection> {
        let mut seen: BTreeSet<RowId> = BTreeSet::new();
        // §14.6 cross-source soundness over an UNBOUNDED recurring series: such a
        // series is infinite, so the enumeration below can only cover a bounded
        // prefix and cannot witness a collision that first appears past it (two
        // sources whose grids align at an offset — s1's period 3 against s2's period
        // 0). This is decided GRID-AWARE, not by holding `$from` constant: two
        // sources collide iff they share the same key under EQUAL `$source`/`$index`/
        // `$until` (they group under one `probe_identity`) AND their `$from` grids can
        // actually coincide. For two fixed-period grids `{φ + k·P}` the offset `φ1−φ2`
        // must be a multiple of `gcd(P1, P2)` for any common `$from` to exist; a
        // phase-offset that is NOT such a multiple makes the grids DISJOINT, so a
        // `$from`-bearing key stays unique and the series must load. `grids` collects,
        // per `probe_identity`, each unbounded source's phase/period so a new source is
        // rejected only against a prior one whose grid can provably meet it.
        let mut grids: BTreeMap<RowId, Vec<GridPhase>> = BTreeMap::new();
        for source_row in self.source_rows(inputs) {
            let Some((from, until, repeat)) = self.bounds(inputs, &source_row) else { continue };
            // A minimal horizon (the start itself): a non-advancing step is caught on
            // the first advance and an invalid bound before any generation, so this
            // never enumerates a long series just to validate it.
            if let Err(err) = recurring_intervals(from, until, repeat.as_ref(), from) {
                return Err(Rejection::new(
                    RejectionReason::Evaluation,
                    format!("source-backed bucket `{}`: {err}", self.name),
                ));
            }
            if self.key.is_none() {
                // The inferred identity is unique by construction (source identity,
                // extended with `$from` when the bucket recurs), so only a custom key
                // needs a data-dependent uniqueness pass.
                continue;
            }
            // §14.5/§14.6: an unbounded recurring series is read only through a
            // bounded selector, but its uniqueness invariant is over EVERY generated
            // row — enumeration cannot prove that, so cross-source uniqueness is proven
            // structurally (see `grids` above). A bounded (finite `$until`) or
            // non-recurring series is generated in full by the enumeration below, which
            // is complete, so it needs no grid analysis (and a legitimate finite
            // `["$from"]` grid whose starts never coincide must still load).
            if let (Some(period), None) = (repeat.as_ref(), until) {
                let phase = GridPhase {
                    from,
                    period: period.clone(),
                    from_identity: self.key_has_from_identity(inputs, &source_row, from),
                };
                let probe_id = self.probe_identity(inputs, &source_row);
                if let Some(prior) = grids.get(&probe_id) {
                    // Same key under a common period binding: the two sources can only
                    // be told apart by `$from`, so they collide unless their grids are
                    // PROVABLY disjoint (a phase offset that no shared boundary can
                    // bridge). Reject against the first prior grid that can meet this one.
                    if prior.iter().any(|other| !phase.provably_disjoint(other)) {
                        return Err(Rejection::new(
                            RejectionReason::DuplicateKey,
                            format!(
                                "source-backed bucket `{}`: custom `$key` is not provably unique \
                                 across an unbounded recurring series — two sources resolve the same \
                                 key and their recurrence grids can align at a shared boundary, so \
                                 they generate two rows with one key; add a source-distinguishing \
                                 component such as `$source.<key>` (§14.5/§14.6)",
                                self.name
                            ),
                        ));
                    }
                }
                grids.entry(probe_id).or_default().push(phase);
            }
            // §14.6 uniqueness: enumerate this source row's generated rows and reject
            // the transition the moment two rows resolve the same custom key. This is
            // complete for a finite series (including cross-source, via the shared
            // `seen` set) and catches an unbounded key that fails to vary per period
            // (a missing `$from`/`$index` component collides at the first advance).
            let horizon = uniqueness_horizon(from, until, repeat.as_ref());
            let Ok(intervals) = recurring_intervals(from, until, repeat.as_ref(), horizon) else {
                continue;
            };
            for interval in intervals {
                if !seen.insert(self.generated_identity(inputs, &source_row, interval)) {
                    return Err(Rejection::new(
                        RejectionReason::DuplicateKey,
                        format!(
                            "source-backed bucket `{}`: custom `$key` is not unique — two \
                             generated rows resolve the same key (§14.6)",
                            self.name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The identity the custom `$key` resolves for `source_row` under a COMMON
    /// synthetic period binding — the §14.6 cross-source GROUPING key on an unbounded
    /// recurring series ([`Self::validate`]). Fixing `$from`/`$until`/`$index` to the
    /// same values for every source makes the key depend only on `$source`, so two
    /// sources that resolve the same identity here are told apart by nothing but
    /// `$from`; whether they actually collide is then decided by their grids
    /// ([`GridPhase::provably_disjoint`]). Mirrors [`Self::generated_identity`] with a
    /// constant interval.
    fn probe_identity(&self, inputs: &BucketInputs<'_>, source_row: &Row) -> RowId {
        let common = Interval { index: 0, from: inputs.now, until: None };
        self.generated_identity(inputs, source_row, common)
    }

    /// Whether the custom `$key` has a component that is the IDENTITY on `$from` —
    /// its value equals `$from` and is insensitive to `$index`/`$until` (§14.6).
    ///
    /// This is the soundness gate for the disjoint-grid accept: only a component
    /// that IS `$from` (not a coarser function of it, and not `$index`, which would
    /// still collide across phase-offset grids at equal indices) guarantees that
    /// disjoint `$from` grids yield disjoint keys. It is decided by evaluation, not
    /// AST introspection (the typed AST is crate-private): a component is treated as
    /// `$from`-identity when it evaluates to exactly `$from` at three distinct
    /// timestamps and is unchanged by varying `$index` and `$until`. A non-identity
    /// expression coinciding with `$from` at three points is not realizable without
    /// being `$from`, so this is sound in practice; a false negative merely falls to
    /// the conservative reject.
    fn key_has_from_identity(&self, inputs: &BucketInputs<'_>, source_row: &Row, from: Timestamp) -> bool {
        let Some(components) = &self.key else { return false };
        let precision = from.precision();
        let base = from.count();
        let ts = |offset: i128| Timestamp::new(base.wrapping_add(offset), precision);
        let (f1, f2, f3, u) = (ts(1), ts(1_000), ts(7_777), ts(123_456));
        components.iter().any(|component| {
            let at = |from: Timestamp, until: Option<Timestamp>, index: i64| {
                self.eval_key_component(inputs, source_row, component, from, until, index)
            };
            at(f1, None, 0) == Value::Timestamp(f1)
                && at(f2, None, 0) == Value::Timestamp(f2)
                && at(f3, None, 0) == Value::Timestamp(f3)
                && at(f1, None, 3) == Value::Timestamp(f1)
                && at(f1, Some(u), 0) == Value::Timestamp(f1)
        })
    }

    /// Evaluate one custom-`$key` component against `source_row` under an explicit
    /// `$from`/`$until`/`$index` binding (§14.4/§14.6). Mirrors
    /// [`Self::generated_identity`]'s structural env for a single component; a
    /// component that does not evaluate to a scalar yields `none`.
    fn eval_key_component(
        &self,
        inputs: &BucketInputs<'_>,
        source_row: &Row,
        component: &TypedExpr,
        from: Timestamp,
        until: Option<Timestamp>,
        index: i64,
    ) -> Value {
        let structurals = BTreeMap::from([
            (SOURCE_CELL[1..].to_owned(), Cell::Row(Box::new(source_row.clone()))),
            (FROM_CELL[1..].to_owned(), Cell::Scalar(Value::Timestamp(from))),
            (UNTIL_CELL[1..].to_owned(), Cell::Scalar(until.map_or(Value::None, Value::Timestamp))),
            (INDEX_CELL[1..].to_owned(), Cell::Scalar(Value::Int(Integer::from(index)))),
        ]);
        let env = self.env(inputs, structurals);
        match component.evaluate(&env, &keyless_current()) {
            Ok(Cell::Scalar(value)) => value,
            _ => Value::None,
        }
    }

    /// The identity a generated row would take, for the §14.6 custom-key uniqueness
    /// pass. Mirrors [`Self::build_row`]'s structural env so a custom `$key`
    /// evaluates against the same `$source`/`$from`/`$until`/`$index` bindings.
    fn generated_identity(&self, inputs: &BucketInputs<'_>, source_row: &Row, interval: Interval) -> RowId {
        let structurals = BTreeMap::from([
            (SOURCE_CELL[1..].to_owned(), Cell::Row(Box::new(source_row.clone()))),
            (FROM_CELL[1..].to_owned(), Cell::Scalar(Value::Timestamp(interval.from))),
            (UNTIL_CELL[1..].to_owned(), Cell::Scalar(interval.until.map_or(Value::None, Value::Timestamp))),
            (INDEX_CELL[1..].to_owned(), Cell::Scalar(Value::Int(Integer::from(interval.index)))),
        ]);
        let env = self.env(inputs, structurals);
        let current = keyless_current();
        self.identity(&env, &current, source_row, interval.from).0
    }
}

/// The recurrence phase of one unbounded source row: its `$from` start, its
/// `$repeat` period, and whether the custom key has a `$from`-identity component
/// (§14.6). Two same-`probe_identity` sources collide unless their grids are
/// provably disjoint.
struct GridPhase {
    from: Timestamp,
    period: Period,
    from_identity: bool,
}

impl GridPhase {
    /// Whether this grid and `other` PROVABLY never share a `$from` boundary — so a
    /// `$from`-bearing key stays unique across them (§14.6).
    ///
    /// Sound-but-conservative: it proves disjointness only for two FIXED-period grids
    /// at the same precision whose key carries a `$from`-identity component. Two such
    /// grids `{φ + k·P}` share a boundary iff the phase offset `(φ1 − φ2)` is a
    /// multiple of `gcd(P1, P2)`; a non-multiple offset is disjoint. Any other shape
    /// (a calendar period, a differing precision, or a key without a `$from`-identity
    /// component — e.g. one keyed on `$index`, which repeats across phase-offset
    /// grids) is NOT proven disjoint, so the caller conservatively rejects.
    fn provably_disjoint(&self, other: &GridPhase) -> bool {
        if !(self.from_identity && other.from_identity) {
            return false;
        }
        let (Period::Fixed(a), Period::Fixed(b)) = (&self.period, &other.period) else {
            return false;
        };
        if self.from.precision() != other.from.precision() {
            return false;
        }
        let per_tick = 1_000_000_000 / self.from.precision().ticks_per_second();
        let (step_a, step_b) = (a.as_nanos() / per_tick, b.as_nanos() / per_tick);
        if step_a <= 0 || step_b <= 0 {
            return false;
        }
        let offset = self.from.count() - other.from.count();
        offset.rem_euclid(gcd(step_a, step_b)) != 0
    }
}

/// The greatest common divisor of two positive tick counts (Euclid), for the §14.6
/// grid-alignment test.
fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a.rem_euclid(b));
    }
    a
}

/// The generation horizon a §14.6 uniqueness pass enumerates one source row over.
///
/// A finite series (bounded `$until`, or a single non-recurring interval) is
/// generated in full regardless of the horizon, so the start itself suffices. An
/// unbounded recurring series is infinite, but a custom key that fails to vary per
/// period repeats within the first two periods; the second boundary is the smallest
/// horizon that exposes such an intra-source collision (a start below it keeps the
/// second interval). A key that varies per period yet still collides across two
/// sources whose grids align at an offset is caught soundly — before this bounded
/// enumeration ever reaches the offending boundary — by the source-distinguishing
/// probe in [`CompiledSourceBucket::validate`], not by widening this horizon.
fn uniqueness_horizon(from: Timestamp, until: Option<Timestamp>, repeat: Option<&Period>) -> Timestamp {
    match repeat {
        Some(period) if until.is_none() => period.advance_from(from, 2).unwrap_or(from),
        _ => from,
    }
}

/// A regenerable view of every source-backed bucket's full extant interval set
/// (§14.2, §14.5).
///
/// A temporal selector reading an unbounded recurring bucket must generate the
/// series far enough to cover its OWN bound (§14.5): the precomputed working set
/// is generated only up to the request clock, so a future `.$at`/`.$between`
/// would otherwise observe no period past `now`. This captures everything a
/// materialization needs except the generation horizon, which the selector
/// supplies through [`Self::extant_to`] — a past-or-present read keeps the clock
/// horizon (behaviour unchanged), a future read extends it to the selector's own
/// bound. It carries no horizon of its own, so it never enumerates an unbounded
/// series outside a bounded selector (the case §14.5 rejects).
#[derive(Clone)]
pub(crate) struct SourceBucketHorizon<'a> {
    buckets: &'a [CompiledSourceBucket],
    base_root: Row,
    params: BTreeMap<String, Cell>,
    context: BTreeMap<String, Cell>,
    now: Timestamp,
    seed: u64,
    keyrings: Vec<KeyringSnapshot>,
}

impl<'a> SourceBucketHorizon<'a> {
    /// Capture the materialization context of `buckets` from `inputs`. Returns
    /// `None` when there is no source-backed bucket, so a package without one
    /// carries no horizon and a temporal selector regenerates nothing.
    pub(crate) fn capture(
        buckets: &'a [CompiledSourceBucket],
        inputs: &BucketInputs<'_>,
    ) -> Option<Self> {
        if buckets.is_empty() {
            return None;
        }
        Some(Self {
            buckets,
            base_root: inputs.base_root.clone(),
            params: inputs.params.clone(),
            context: inputs.context.clone(),
            now: inputs.now,
            seed: inputs.seed,
            keyrings: inputs.keyrings.to_vec(),
        })
    }

    /// The full extant interval set of every source-backed bucket, each generated
    /// up to `horizon` (§14.2): the working set a temporal selector re-derives
    /// activity over. `horizon` is driven by the selector's own bound (§14.5), so a
    /// read past the clock still generates the periods that cover it, while a
    /// bounded finite series is generated in full regardless of `horizon`.
    pub(crate) fn extant_to(&self, horizon: Timestamp) -> Vec<NamedExtant> {
        let inputs = BucketInputs {
            base_root: &self.base_root,
            params: &self.params,
            context: &self.context,
            now: self.now,
            seed: self.seed,
            keyrings: &self.keyrings,
        };
        // §7.1: tag each source-backed bucket's regenerated extant with its
        // collection name so a temporal selector addressing it resolves by name.
        self.buckets
            .iter()
            .map(|bucket| NamedExtant {
                name: bucket.name.clone(),
                rows: bucket.materialize(&inputs, horizon, false),
            })
            .collect()
    }
}

/// The keyless `.` a bucket/output expression is evaluated against (§14.4): its
/// inputs come from the structural bindings, not a stored receiver.
fn keyless_current() -> Cell {
    Cell::Row(Box::new(Row::keyless(RowId::leaf(0), Vec::new())))
}

/// Whether the half-open interval `[from, until)` is active at `now` (§14.1).
fn active_at(from: Timestamp, until: Option<Timestamp>, now: Timestamp) -> bool {
    now >= from && until.is_none_or(|u| now < u)
}

/// The application-visible key value of a composite custom key: the lone component
/// for a single-field key, or a positional struct otherwise.
fn composite_value(values: &[Value]) -> Value {
    match values {
        [single] => single.clone(),
        _ => Value::Struct(Struct::new(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (Text::new(index.to_string()), value.clone())),
        )),
    }
}

/// Canonical key text for a custom key's components, for the derived row's stable
/// identity (Annex D.2), with a total fallback for a non-key-eligible component.
fn key_text(values: &[Value]) -> String {
    match liasse_ident::KeyText::from_key_values(values) {
        Ok(text) => text.as_str().to_owned(),
        Err(_) => values
            .iter()
            .map(Value::to_canonical_json_string)
            .collect::<Vec<_>>()
            .join(":"),
    }
}

