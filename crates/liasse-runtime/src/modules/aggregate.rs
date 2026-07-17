//! The evaluated module-space aggregation the root engine folds into a
//! `.modules::iface` read (§13.9).
//!
//! The installed child instances live in the [`ModuleHost`](crate::ModuleHost),
//! not in the root engine's own committed state, so a root-package expression that
//! addresses `.modules[k]::iface` or aggregates `.modules::iface` cannot reach them
//! by ordinary materialization. The host reads each enabled child's exposed
//! interface `$view` through the boundary — only the projected fields cross, so a
//! private field stays unreachable (§13.8 isolation) — and hands the root engine a
//! [`ModuleAggregate`]: a snapshot keyed by module-space display path, each space
//! carrying its enabled instances and the interface rows they expose.
//!
//! The engine then folds this snapshot into the package-root row so the existing
//! `::` traversal and projection machinery (§6.4, §7.1) evaluates the aggregation
//! with no change to the expression layer: a `$modules` space becomes a keyed
//! collection of instance rows (keyed by instance name, so `modules.$key` is the
//! instance identity, §13.9), each instance row carrying one cell per exposed
//! interface whose rows are the boundary-projected rows (keeping their own key, so
//! `iface.$key` is the exposed row identity).

use std::collections::BTreeMap;

use liasse_expr::{Cell, Row, RowId};
use liasse_ident::KeyText;
use liasse_value::{Text, Value};

use crate::eval::with_cell;

/// One enabled child instance in a module space, with the interface rows it
/// exposes through the boundary (§13.8/§13.9).
pub(crate) struct AggregatedInstance {
    /// The instance name — the local component of instance identity and the
    /// `modules.$key` value a §13.9 aggregation projects (§13.3).
    pub(crate) name: String,
    /// The exposed interface rows, one entry per readable interface the child
    /// declares, each carrying the rows the interface `$view` projected. Only the
    /// projected fields are present, so a private child field is absent here
    /// (§13.8 isolation).
    pub(crate) interfaces: Vec<(String, Vec<Row>)>,
}

/// The evaluated module aggregation for one root-engine read (§13.9): the enabled
/// instances of every module space, keyed by the space's display path
/// (`/companies/acme/modules`). Built by the [`ModuleHost`](crate::ModuleHost),
/// which owns the children; consumed by the root engine, which folds each space
/// into the containing rows so `.modules::iface` resolves.
pub(crate) struct ModuleAggregate {
    spaces: BTreeMap<String, Vec<AggregatedInstance>>,
}

impl ModuleAggregate {
    /// An aggregation over the enabled instances grouped by space display path.
    pub(crate) fn new(spaces: BTreeMap<String, Vec<AggregatedInstance>>) -> Self {
        Self { spaces }
    }

    /// The enabled instances installed in the space at `display_path`, if any.
    fn instances(&self, display_path: &str) -> Option<&[AggregatedInstance]> {
        self.spaces.get(display_path).map(Vec::as_slice)
    }

    /// Fold this aggregation into the package-root row for every `$modules` space
    /// declared at `spaces` (each a declaration-name path from `$model`). A
    /// root-level space (`["modules"]`) injects one keyed instance collection onto
    /// the root; a row-scoped space (`["companies", "modules"]`) injects one onto
    /// each row of the containing collection, keyed by that row's display path
    /// (§13.2 "an installation space at its exact location"). A space with no
    /// enabled instance injects an empty collection so `.modules::iface` reads as an
    /// empty stream rather than faulting. Declaration paths deeper than one
    /// containing collection are a documented seam (left unfolded).
    pub(crate) fn fold_into<'p>(
        &self,
        root: Row,
        spaces: impl IntoIterator<Item = &'p [String]>,
    ) -> Row {
        let mut root = root;
        for path in spaces {
            root = self.inject_space(root, path);
        }
        root
    }

    fn inject_space(&self, root: Row, path: &[String]) -> Row {
        match path {
            [member] => {
                let display = format!("/{member}");
                with_cell(root, member, self.space_cell(&display))
            }
            [collection, member] => {
                let Some(Cell::Collection(rows)) = root.cell(collection).cloned() else {
                    return root;
                };
                let injected: Vec<Row> = rows
                    .into_iter()
                    .map(|row| self.inject_row(row, collection, member))
                    .collect();
                with_cell(root, collection, Cell::Collection(injected))
            }
            // A `$modules` space nested under more than one containing collection is
            // a documented seam: the display-path derivation would have to interleave
            // every ancestor key, which no CORE corpus case exercises.
            _ => root,
        }
    }

    fn inject_row(&self, row: Row, collection: &str, member: &str) -> Row {
        let Ok(key) = KeyText::from_key_values(std::slice::from_ref(row.key())) else {
            return row;
        };
        let display = format!("/{collection}/{}/{member}", key.as_str());
        with_cell(row, member, self.space_cell(&display))
    }

    /// The keyed instance collection cell for the space at `display_path`: one row
    /// per enabled instance (keyed by instance name), each carrying its exposed
    /// interface cells. An absent or empty space yields an empty collection.
    fn space_cell(&self, display_path: &str) -> Cell {
        let instances = self.instances(display_path).unwrap_or(&[]);
        Cell::Collection(instances.iter().map(instance_row).collect())
    }
}

/// The instance row for a §13.9 aggregation: keyed by the instance name (so
/// `modules.$key` reads the instance identity, §13.3) and carrying one cell per
/// exposed interface, each the boundary-projected rows as a nested collection so
/// `::iface` traverses them (§6.4).
fn instance_row(instance: &AggregatedInstance) -> Row {
    let cells = instance
        .interfaces
        .iter()
        .map(|(name, rows)| (name.clone(), Cell::Collection(rows.clone())));
    Row::new(
        RowId::keyed(instance.name.clone()),
        Value::Text(Text::new(instance.name.clone())),
        cells,
    )
}
