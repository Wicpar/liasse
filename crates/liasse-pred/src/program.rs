//! [`RowPrograms`] — the sole implementor of [`ViewProgram`], evaluating the three
//! logical faces (admit / project / sort) of one lowered view read through the
//! shared `liasse-expr` interpreter (`TypedExpr::evaluate_bound`).
//!
//! Each face rebuilds the candidate [`Row`] from the descriptor, then reproduces
//! the interpreter's own per-scope evaluation exactly (§7.2, `views.rs`):
//! `select_bind_scopes` for the admit (the row bound to the filter name, kept iff
//! `Bool(true)`), `project_row` for the projection (each output in dependency
//! order, earlier outputs bound for later ones), and `eval_keys` for the sort tuple
//! (the projected outputs and the bind visible, `.` the source row). Because the
//! evaluation is the same linked interpreter over an identically-rebuilt candidate,
//! parity with the interpreter is by construction (§7.3).

use liasse_expr::wire::{env_to_wire, to_wire};
use liasse_expr::{Cell, EvalError, TypedExpr};
use liasse_store::{CandidateSubtree, EvalFault, KeyValue, SortDirection, ViewProgram};
use liasse_value::{Struct, Text, Value};

use crate::descriptor::{coverage_field_value, field_value, CandidateDescriptor};
use crate::penv::{is_true, ProgEnv};

/// How a program's projection exposes its output cells (§7.2): a flat view keeps a
/// keyless nested projection as an inline `Value::Struct` (`cell_field_value`);
/// §10.5 coverage keeps only scalar cells (`row_object`), since the nested keyed
/// view is re-added by the descent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectShape {
    /// A flat `$view` projection.
    Flat,
    /// A §10.5 coverage projection.
    Coverage,
}

/// The lowered, hoisted residual faces of one view read — the parts the runtime
/// assembles into a [`RowPrograms`]. `admit` is the composed filter (`None` admits
/// every row); `outputs` are the projection outputs in dependency order; `sort`
/// pairs each residual sort key with its direction; `env` is the shared hoisted
/// candidate-free values; `bind` is the filter/coverage bind name.
pub struct RowProgramsParts {
    /// The composed admit filter (`None` admits every row).
    pub admit: Option<TypedExpr>,
    /// The projection outputs in dependency order.
    pub outputs: Vec<(String, TypedExpr)>,
    /// Each residual sort key with its direction, highest priority first.
    pub sort: Vec<(TypedExpr, SortDirection)>,
    /// The shared hoisted candidate-free values, keyed by synthetic name.
    pub env: Vec<(String, Cell)>,
    /// The filter/coverage bind name.
    pub bind: Option<String>,
    /// The candidate shape.
    pub descriptor: CandidateDescriptor,
    /// The nested-collection step names the faces read through the candidate.
    pub subtree_steps: Vec<String>,
    /// How the projection exposes its output cells.
    pub shape: ProjectShape,
}

/// One lowered view read's compiled per-row faces.
pub struct RowPrograms {
    admit: Option<TypedExpr>,
    outputs: Vec<(String, TypedExpr)>,
    sort: Vec<TypedExpr>,
    directions: Vec<SortDirection>,
    env: Vec<(String, Cell)>,
    bind: Option<String>,
    descriptor: CandidateDescriptor,
    subtree_steps: Vec<String>,
    shape: ProjectShape,
    admit_wire: Option<Vec<u8>>,
    project_wire: Vec<u8>,
    sort_wire: Option<Vec<u8>>,
    env_wire: Vec<u8>,
}

/// The version-lock constant (§7.7): producer and consumer of the eval wire MUST
/// carry the identical string, or the ABI handshake refuses the extension.
pub const EVAL_ABI: &str = "liasse-eval-wire/1";

impl RowPrograms {
    /// Assemble a program from its lowered, hoisted residual faces.
    ///
    /// # Errors
    ///
    /// Errors when a residual face or the hoisted env fails to serialize to the
    /// version-locked wire (§7.4) — unreachable for an audited residual.
    pub fn new(parts: RowProgramsParts) -> Result<Self, liasse_expr::wire::WireError> {
        let RowProgramsParts { admit, outputs, sort, env, bind, descriptor, subtree_steps, shape } =
            parts;
        let admit_wire = admit.as_ref().map(to_wire).transpose()?;
        let project_wire = postcard::to_allocvec(&outputs).map_err(codec)?;
        let (sort, directions): (Vec<TypedExpr>, Vec<SortDirection>) = sort.into_iter().unzip();
        let sort_wire = if sort.is_empty() {
            None
        } else {
            Some(postcard::to_allocvec(&sort).map_err(codec)?)
        };
        let env_wire = env_to_wire(&env)?;
        Ok(Self {
            admit,
            outputs,
            sort,
            directions,
            env,
            bind,
            descriptor,
            subtree_steps,
            shape,
            admit_wire,
            project_wire,
            sort_wire,
            env_wire,
        })
    }

    /// Evaluate all projection outputs over `candidate`, in dependency order, each
    /// earlier output bound for later ones (`project_row`). Returns the output
    /// cells in declaration order.
    fn project_cells(&self, candidate: &Cell) -> Result<Vec<(String, Cell)>, EvalError> {
        let env = self.env_for(candidate);
        let mut frame: Vec<(String, Cell)> = self.base_frame(candidate);
        let mut cells = Vec::with_capacity(self.outputs.len());
        for (name, expr) in &self.outputs {
            let cell = expr.evaluate_bound(&env, candidate, frame.clone())?;
            // §7.1/§6.4: an output never shadows the row/loop bind; otherwise it is
            // bound so a later output (and the sort keys) can read it.
            if self.bind.as_deref() != Some(name.as_str()) {
                frame.push((name.clone(), cell.clone()));
            }
            cells.push((name.clone(), cell));
        }
        Ok(cells)
    }

    /// The base frame bindings for a candidate: the filter/coverage bind name
    /// resolved to the candidate row (seeding a `LocalBinding` reference).
    fn base_frame(&self, candidate: &Cell) -> Vec<(String, Cell)> {
        match &self.bind {
            Some(bind) => vec![(bind.clone(), candidate.clone())],
            None => Vec::new(),
        }
    }

    /// The environment for one candidate: the shared hoisted env plus the bind name
    /// resolved to this candidate (seeding a `ScopeBinding` reference).
    fn env_for<'a>(&'a self, candidate: &'a Cell) -> ProgEnv<'a> {
        ProgEnv::new(&self.env, self.bind.as_deref().map(|bind| (bind, candidate)))
    }

    fn candidate(&self, value: &Value, key: &KeyValue, subtree: &CandidateSubtree) -> Cell {
        Cell::Row(Box::new(self.descriptor.build_row(value, key, subtree)))
    }
}

impl ViewProgram for RowPrograms {
    fn subtree_steps(&self) -> &[String] {
        &self.subtree_steps
    }

    fn admits(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<bool, EvalFault> {
        let Some(condition) = &self.admit else {
            return Ok(true);
        };
        let candidate = self.candidate(value, key, subtree);
        let env = self.env_for(&candidate);
        let verdict = condition
            .evaluate_bound(&env, &candidate, self.base_frame(&candidate))
            .map_err(fault)?;
        Ok(is_true(&verdict))
    }

    fn project(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<Value, EvalFault> {
        let candidate = self.candidate(value, key, subtree);
        let cells = self.project_cells(&candidate).map_err(fault)?;
        let expose = match self.shape {
            ProjectShape::Flat => field_value,
            ProjectShape::Coverage => coverage_field_value,
        };
        let members = cells
            .iter()
            .filter_map(|(name, cell)| Some((Text::new(name.clone()), expose(cell)?)));
        Ok(Value::Struct(Struct::new(members)))
    }

    fn sort_tuple(
        &self,
        value: &Value,
        key: &KeyValue,
        subtree: &CandidateSubtree,
    ) -> Result<Vec<Value>, EvalFault> {
        if self.sort.is_empty() {
            return Ok(Vec::new());
        }
        let candidate = self.candidate(value, key, subtree);
        let cells = self.project_cells(&candidate).map_err(fault)?;
        let env = self.env_for(&candidate);
        // §7.3: sort keys see the bind and every projected output (non-shadowing),
        // with `.` the source row.
        let mut frame = self.base_frame(&candidate);
        for (name, cell) in &cells {
            if self.bind.as_deref() != Some(name.as_str()) {
                frame.push((name.clone(), cell.clone()));
            }
        }
        let mut tuple = Vec::with_capacity(self.sort.len());
        for key_expr in &self.sort {
            let cell = key_expr.evaluate_bound(&env, &candidate, frame.clone()).map_err(fault)?;
            match cell.as_scalar() {
                Some(scalar) => tuple.push(scalar.clone()),
                None => return Err(EvalFault::new("a `$sort` key must evaluate to a scalar (§7.3)")),
            }
        }
        Ok(tuple)
    }

    fn sort_directions(&self) -> &[SortDirection] {
        &self.directions
    }

    fn admit_wire(&self) -> Option<&[u8]> {
        self.admit_wire.as_deref()
    }

    fn project_wire(&self) -> &[u8] {
        &self.project_wire
    }

    fn sort_wire(&self) -> Option<&[u8]> {
        self.sort_wire.as_deref()
    }

    fn env_wire(&self) -> &[u8] {
        &self.env_wire
    }
}

fn fault(error: EvalError) -> EvalFault {
    EvalFault::new(format!("{error:?}"))
}

fn codec(error: postcard::Error) -> liasse_expr::wire::WireError {
    liasse_expr::wire::WireError::Codec(error.to_string())
}
