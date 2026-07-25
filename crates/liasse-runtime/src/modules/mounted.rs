//! The rows of a module collection, as the root engine materializes them (§13.2).
//!
//! A module collection is an ordinary map (`{ $key: text, $value: module }`)
//! whose entries are the host's mounted instances rather than stored rows — the
//! same arrangement a source-backed bucket has (§14.4), and for the same reason:
//! the rows are derived, so the store holds none and the engine folds them in at
//! materialization.
//!
//! Each entry materializes as the ordinary map row — `$key` the instance name,
//! `$value` the move-only [`Value::Module`] handle — plus one cell per readable
//! interface the instance exposes, each carrying the boundary-projected rows
//! (§13.8: only projected fields cross, so a private field stays unreachable).
//! Nothing here is a module-specific *addressing* scheme: `.modules[@id]` is map
//! indexing, `modules.$key` is the map key, and `.modules::iface` is the §6.4
//! nested traversal every collection has. Folding an entry into its containing
//! row is done by walking the entry's own [`RowAddress`], so containment reaches
//! any nesting depth the containing collections give.

use liasse_expr::{Cell, Row, RowId, MAP_KEY, MAP_VALUE};
use liasse_store::RowAddress;
use liasse_value::{ModuleHandle, Value};

use crate::eval::with_cell;
use crate::modules::address;

/// One mounted instance, ready to materialize as a module-collection row.
struct MountedEntry {
    /// The address of the map entry — the instance's identity (§13.3).
    at: RowAddress,
    /// The exposed interface rows, one entry per readable interface the child
    /// declares. Only the projected fields are present, so a private child field
    /// is absent here (§13.8 isolation).
    interfaces: Vec<(String, Vec<Row>)>,
}

/// Every enabled module-collection entry the host holds, for one root-engine
/// read. Built by the [`ModuleHost`](crate::ModuleHost), which owns the children;
/// consumed by the root engine, which folds each entry into the row that contains
/// its collection.
#[derive(Default)]
pub(crate) struct MountedModules {
    entries: Vec<MountedEntry>,
}

impl MountedModules {
    /// Record one mounted instance's entry.
    pub(crate) fn push(&mut self, at: &RowAddress, interfaces: Vec<(String, Vec<Row>)>) {
        self.entries.push(MountedEntry { at: at.clone(), interfaces });
    }

    /// Fold every module collection declared at `paths` into `root`, replacing
    /// each with its live entries.
    ///
    /// A path is a declaration-name path from `$model` (`["companies",
    /// "modules"]`). The walk descends the containing collections by *matching
    /// each row's own key* against the entry address's key at that level, so a
    /// collection nested under any number of containing rows is reached — the
    /// containment is the containing collections', not a scheme of its own.
    pub(crate) fn fold_into<'p>(
        &self,
        root: Row,
        paths: impl IntoIterator<Item = &'p [String]>,
    ) -> Row {
        let mut root = root;
        for path in paths {
            root = self.inject(root, path, 0, &[]);
        }
        root
    }

    /// Inject the module collection reached by `path` under a row whose ancestor
    /// keys are `keys` (root-first, one per descended level). `path` stays whole so
    /// the match below sees the ancestor collection NAMES as well as their keys.
    fn inject(&self, row: Row, path: &[String], depth: usize, keys: &[Value]) -> Row {
        match path.get(depth..) {
            None | Some([]) => row,
            // The last segment names the module collection itself.
            Some([own]) => with_cell(row, own, self.entry_cell(path, keys)),
            Some([collection, ..]) => {
                let Some(Cell::Collection(rows)) = row.cell(collection).cloned() else {
                    return row;
                };
                let injected: Vec<Row> = rows
                    .into_iter()
                    .map(|nested| {
                        let mut keys = keys.to_vec();
                        keys.push(nested.key().clone());
                        self.inject(nested, path, depth + 1, &keys)
                    })
                    .collect();
                with_cell(row, collection, Cell::Collection(injected))
            }
        }
    }

    /// The rows of the module collection `path` declares, under the containing rows
    /// keyed by `keys`: one per mounted instance whose address matches that
    /// containment exactly. An empty collection is a legitimate answer, so a module
    /// collection with no instance reads as an empty stream.
    fn entry_cell(&self, path: &[String], keys: &[Value]) -> Cell {
        let rows = self.entries.iter().filter(|entry| entry_is_at(entry, path, keys)).map(entry_row).collect();
        Cell::Collection(rows)
    }
}

/// Whether `entry` is an entry of the module collection `path` declares, under the
/// containing rows keyed by `keys`.
///
/// The whole address is compared, step by step: every collection NAME must equal
/// the declaration path's at that level AND every containing key must equal the row
/// the walk descended through. Both halves are load-bearing — two module
/// collections may share a trailing declaration name under different parents
/// (`companies.modules` and `divisions.modules`), and if their containing rows also
/// happen to share a key, matching on keys alone would fold one collection's
/// instances into the other's read: a wrong module address that looks correct.
fn entry_is_at(entry: &MountedEntry, path: &[String], keys: &[Value]) -> bool {
    let steps: Vec<_> = entry.at.steps().collect();
    steps.len() == path.len()
        && steps.iter().zip(path).all(|(step, name)| step.name().as_str() == name)
        && steps.len() == keys.len() + 1
        && steps
            .iter()
            .zip(keys)
            .all(|(step, key)| &address::step_key_value(step) == key)
}

/// One module-collection row: the §5.4 map row (`$key` the instance name,
/// `$value` the module handle) plus one cell per exposed interface, each the
/// boundary-projected rows as a nested collection so `::iface` traverses them
/// (§6.4).
fn entry_row(entry: &MountedEntry) -> Row {
    let name = address::instance_name(&entry.at).unwrap_or_default().to_owned();
    let key = Value::Text(liasse_value::Text::new(name.clone()));
    let cells = [
        (MAP_KEY.to_owned(), Cell::Scalar(key.clone())),
        // §13.16: the entry's value IS the module — a move-only handle addressed
        // by the entry's own row address, which is what a `<-` relocate moves and
        // what a host-lent `module` parameter carries.
        (
            MAP_VALUE.to_owned(),
            Cell::Scalar(Value::Module(ModuleHandle::Mounted(entry.at.render()))),
        ),
    ]
    .into_iter()
    .chain(
        entry
            .interfaces
            .iter()
            .map(|(interface, rows)| (interface.clone(), Cell::Collection(rows.clone()))),
    );
    Row::new(RowId::keyed(name), key, cells)
}
