//! The embedded, versioned schema and its per-instance namespace.
//!
//! Each package instance owns one PostgreSQL schema, so instances are isolated
//! and droppable as a unit (SPEC §23.3 grants this physical freedom). The name is
//! derived from the instance identity — sanitized to a safe identifier and folded
//! with a stable hash so it stays within PostgreSQL's 63-byte limit — under a
//! caller-supplied namespace token that guarantees isolation (a test run's unique
//! suffix; a deployment's fixed prefix).
//!
//! The DDL is `CREATE … IF NOT EXISTS` throughout and records a single
//! `schema_version` row. Opening refuses a schema stamped newer than this code
//! knows: forward compatibility is not assumed.
//!
//! # Enumerable objects
//!
//! The fixed tables ([`Schema::tables`]), the secondary indexes ([`Schema::indexes`])
//! and the sequences ([`Schema::sequences`]) a schema needs are held as *data* rather
//! than baked into one opaque DDL blob, so each set is enumerable. Opening creates
//! every object idempotently (`CREATE … IF NOT EXISTS`), and because the sets are data
//! a later reconciliation round (see [`crate::reconcile`]) can diff the live objects
//! against them and drop any orphan the active model no longer declares — no
//! migration leaves orphaned structures behind. Primary-key indexes and `UNIQUE`
//! *table constraints* are intrinsic to their table declarations (they vanish with
//! the table) and so are not part of the derived index set; a bare
//! `CREATE UNIQUE INDEX` — like the node lookup — is a managed secondary index and
//! is in the set. Identity-column sequences are likewise intrinsic and excluded.

/// The schema version this build writes and understands. Opening a schema with a
/// higher stamp is refused rather than guessed at, and opening one stamped below
/// [`MIN_COMPATIBLE_VERSION`] is refused too — this backend has no in-place column
/// migration, so a physical layout change is a re-create, never a silent mismatch.
///
/// Bumped to 2 when a model-derived key-order index was added; to 3 when the
/// node-adjacency `nodes` table and its `node_key_lookup` unique index became the
/// sole durable row representation (the earlier flat `rows` table was removed); to
/// 4 when `nodes.value`/`nodes.incarnation` became NULLABLE to carry *tombstones* —
/// a deleted non-leaf ancestor kept as a structural-only position so its retained
/// descendants (logical orphans, §5.4) stay addressable, replacing the earlier
/// subtree cascade; to 5 when a `created` column was added to `nodes` (per-row
/// recorded admission instant, §14.1 `$created`/§22.6) and to `commit_log` (the
/// commit's fixed `now()`, §22.5), so a lifecycle bucket's `$created`-defaulted
/// `$from` reads the instant a row was admitted; to **6** when admission stopped
/// assigning serial positions (§22.1: history construction follows committed
/// transitions independently of write admission) — `commit_log` became keyed by the
/// admitting transaction id (`xid`) with a NULLABLE `seq` the history builder
/// ([`crate::history`]) stamps after settlement, `instance_meta` lost both its
/// write-side `head` counter (the built history's tip is now the sole head) and its
/// `next_incarnation` counter (an opaque token, so it moved to a lock-free
/// `SEQUENCE`).
pub const SCHEMA_VERSION: i32 = 6;

/// The oldest stamp this build can open. Versions below it were written with a
/// different physical column layout and this backend carries no `ALTER TABLE`
/// migration path, so opening one is refused with an actionable message instead of
/// failing later, mid-query, on a missing column.
pub const MIN_COMPATIBLE_VERSION: i32 = 6;

/// The single order in which every actor takes relation locks, so two of them can
/// never form a wait cycle.
///
/// It is the order an **admission** touches relations, because that one is not free
/// to change: the residual, then the node tree, then the instance singletons (and
/// those only when the transition changes one). Reconciliation emits its DDL in this
/// order for that reason alone — `CREATE INDEX IF NOT EXISTS` takes a `ShareLock` on
/// its table and holds it for the rest of the transaction *even when the index
/// already exists*, so an opener reconciling an instance while another connection
/// admits to it is two parties each taking two locks, and only a shared order keeps
/// that safe. Declaration order in [`Schema::tables`]/[`Schema::indexes`] is
/// deliberately NOT the contract; this is, and [`Schema::create_ddl`] sorts by it.
///
/// `schema_version` is last here and is nonetheless created first, by
/// [`Schema::version_ddl`], before the stamp can be read. That is not an exception:
/// no admission ever touches `schema_version`, so it can never be one end of a cycle
/// with one, and two concurrent reconcilers take it in the same order as each other.
const LOCK_ORDER: [&str; 6] =
    ["commit_log", "nodes", "instance_meta", "blobs", "history_points", "schema_version"];

/// Where `table` sits in [`LOCK_ORDER`]. An unlisted table sorts last, which keeps
/// the ordering total without a panic; every table this crate declares is listed.
fn lock_rank(table: &str) -> usize {
    LOCK_ORDER.iter().position(|listed| *listed == table).unwrap_or(LOCK_ORDER.len())
}

/// A per-instance schema namespace: a validated PostgreSQL identifier.
#[derive(Debug, Clone)]
pub struct Schema {
    name: String,
}

/// A secondary index one of a [`Schema`]'s tables needs, held as data so the set
/// is enumerable rather than fixed text.
///
/// Its creation is idempotent (`CREATE INDEX IF NOT EXISTS`) and it carries a
/// matching [`drop_sql`](IndexSpec::drop_sql) so the reconciliation lifecycle can
/// create the indexes the active model needs and drop the ones it no longer does,
/// keyed by the deterministic index [`name`](IndexSpec::name).
#[derive(Debug, Clone)]
pub struct IndexSpec {
    name: &'static str,
    table: &'static str,
    key: &'static str,
    unique: bool,
    /// The `WHERE` body of a partial index, when the index only covers a subset of
    /// the table's rows. `None` builds a full index.
    predicate: Option<&'static str>,
}

impl IndexSpec {
    /// The deterministic index name — unique within the schema and stable across
    /// opens, which is what makes create/drop idempotent and reconcilable.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// The table this index is defined on.
    #[must_use]
    pub fn table(&self) -> &str {
        self.table
    }

    /// Whether the index is unique — a `CREATE UNIQUE INDEX`. A unique secondary
    /// index (unlike a `UNIQUE` table constraint) is a bare index this backend
    /// manages and reconciles, so it is declared here as data, not baked into the
    /// table body.
    #[must_use]
    pub fn is_unique(&self) -> bool {
        self.unique
    }

    /// Idempotent creation DDL, scoped to `schema`. A unique index emits
    /// `CREATE UNIQUE INDEX`, so the index doubles as a declared secondary index
    /// (droppable/reconcilable) rather than an intrinsic table constraint.
    #[must_use]
    pub fn create_sql(&self, schema: &Schema) -> String {
        let unique = if self.unique { "UNIQUE " } else { "" };
        let predicate = self.predicate.map_or_else(String::new, |body| format!(" WHERE {body}"));
        format!(
            "CREATE {unique}INDEX IF NOT EXISTS {} ON {}.{} ({}){predicate};",
            quote(self.name),
            schema.quoted(),
            quote(self.table),
            self.key
        )
    }

    /// Idempotent drop DDL, scoped to `schema` — the reconciliation round's tool
    /// for retiring an index the active model no longer needs.
    #[must_use]
    pub fn drop_sql(&self, schema: &Schema) -> String {
        format!("DROP INDEX IF EXISTS {}.{};", schema.quoted(), quote(self.name))
    }
}

/// A sequence a [`Schema`] owns, held as data so the declared set is enumerable and
/// the reconciliation lifecycle can create what is missing and drop what has fallen
/// out of the model.
///
/// A sequence is the right shape for a counter whose **gaps carry no meaning**:
/// `nextval` is non-transactional, so a value drawn by an attempt that later rolls
/// back is burned rather than reused, and it takes no row lock, so drawing one never
/// serializes concurrent writers.
#[derive(Debug, Clone, Copy)]
pub struct SequenceSpec {
    name: &'static str,
}

impl SequenceSpec {
    /// The bare sequence name — its identity within the schema and the key the
    /// reconciler diffs the live catalog against.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Idempotent creation DDL, scoped to `schema`. Starts at zero so the first
    /// drawn value is `0`, matching the reference store's first incarnation token.
    #[must_use]
    pub fn create_sql(&self, schema: &Schema) -> String {
        format!(
            "CREATE SEQUENCE IF NOT EXISTS {}.{} AS BIGINT MINVALUE 0 START WITH 0;",
            schema.quoted(),
            quote(self.name)
        )
    }

    /// The schema-qualified, quoted name a `nextval` call names it by.
    #[must_use]
    pub fn qualified(&self, schema: &Schema) -> String {
        format!("{}.{}", schema.quoted(), quote(self.name))
    }
}

/// One of a [`Schema`]'s fixed tables, held as data so the same list drives both
/// the creating DDL ([`Schema::create_ddl`]) and the reconciliation round's
/// *desired* table set — a single source of truth means the two can never drift.
/// A table present in the instance schema but absent from this list is an orphan
/// (a leftover from an earlier backend layout) the reconciler drops.
#[derive(Debug, Clone, Copy)]
pub struct TableSpec {
    name: &'static str,
    columns: &'static str,
}

impl TableSpec {
    /// The bare table name — its identity within the schema and the key the
    /// reconciler diffs the live catalog against.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Idempotent creation DDL, scoped to `schema`. The primary-key and unique
    /// constraints in the column body materialize the intrinsic indexes the
    /// reconciler preserves.
    ///
    /// A `{schema}` token in the column body is expanded to the quoted schema
    /// name — the one interpolation a column body needs, for a schema-qualified
    /// self-referential foreign key (`nodes.parent_id REFERENCES {schema}.nodes`).
    /// Column bodies without the token are unaffected.
    #[must_use]
    pub fn create_sql(&self, schema: &Schema) -> String {
        let columns = self.columns.replace("{schema}", &schema.quoted());
        format!("CREATE TABLE IF NOT EXISTS {}.{} ({columns});", schema.quoted(), quote(self.name))
    }
}

impl Schema {
    /// Derive the schema for `instance` under `namespace`. Both are sanitized to
    /// `[a-z0-9_]`; the instance label is additionally folded through a stable
    /// hash so distinct identities never collide after truncation.
    #[must_use]
    pub fn derive(namespace: &str, instance: &str) -> Self {
        let ns = sanitize(namespace);
        let label = sanitize(instance);
        let digest = fnv1a(instance.as_bytes());
        // `liasse_<ns>_<label>_<hash>` bounded well under 63 bytes.
        let ns = truncate(&ns, 16);
        let label = truncate(&label, 24);
        Self { name: format!("liasse_{ns}_{label}_{digest:08x}") }
    }

    /// The bare (unquoted) schema identifier.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The schema identifier quoted for interpolation into SQL. Sanitization
    /// already removed every quote character, so this is defence in depth.
    #[must_use]
    pub fn quoted(&self) -> String {
        format!("\"{}\"", self.name.replace('"', "\"\""))
    }

    /// The secondary indexes this schema's tables need, derived from the query
    /// patterns the backend must serve without a sequential scan (see the crate's
    /// index-coverage gates). The set is data, so opening creates each idempotently
    /// and reconciliation can drop any that fall out of the active model.
    ///
    /// Primary-key indexes (`nodes(id)`, `commit_log(seq)`, `blobs(digest)`,
    /// `history_points(lineage, point)`) are intrinsic to the table declarations and
    /// are not listed here — they serve every point lookup and the seq-ordered log
    /// reads directly, and drop with their table.
    #[must_use]
    pub fn indexes(&self) -> Vec<IndexSpec> {
        vec![
            // The node-adjacency point lookup and uniqueness: a row's node is found
            // by `(parent_id, step_name, key_enc)`, and no two sibling rows may share
            // a level key. `key_enc` is the order-preserving `BYTEA` encoding, so this
            // unique index serves both the point lookup and an ordered sibling scan
            // (`WHERE parent_id = ? AND step_name = ? ORDER BY key_enc`) with no sort —
            // `BYTEA` compares by unsigned `memcmp`, so no `COLLATE` is needed. It is a
            // bare `CREATE UNIQUE INDEX` (not a table constraint), so the reconciler
            // manages it as a declared secondary index.
            IndexSpec {
                name: "node_key_lookup",
                table: "nodes",
                key: "parent_id, step_name, key_enc",
                unique: true,
                predicate: None,
            },
            // The history builder's work queue ([`crate::history`]): the settled
            // admissions that still carry no serial position, in admitting-transaction
            // order. Partial on `seq IS NULL`, so it holds only the (normally tiny)
            // unpositioned tail rather than the whole log, and the builder's scan is an
            // index scan over exactly that tail.
            IndexSpec {
                name: "commit_log_unpositioned",
                table: "commit_log",
                key: "xid",
                unique: false,
                predicate: Some("seq IS NULL"),
            },
        ]
    }

    /// The sequences this schema owns, as data so the same list drives the creating
    /// DDL and the reconciler's desired-set.
    ///
    /// One entry: the opaque row-incarnation counter (D.1). It is a `SEQUENCE`
    /// rather than a counter column precisely because incarnations are **opaque
    /// tokens whose gaps are meaningless** — a `nextval` never rolls back, so a
    /// token burned by an aborted staging is never reused (the durable
    /// burn-on-allocate the contract promises), and it takes no row lock, so
    /// allocating one does not serialize concurrent admissions the way the
    /// `instance_meta` counter row it replaced did.
    #[must_use]
    pub fn sequences(&self) -> [SequenceSpec; 1] {
        [SequenceSpec { name: "incarnations" }]
    }

    /// The fixed tables every instance schema owns, as data so the same list
    /// drives the creating DDL and the reconciler's desired-set (§21 retains
    /// `commit_log`/`history_points`/`blobs`; none of the six is ever an orphan).
    ///
    /// The application collections do not each get a table. The `nodes` adjacency
    /// tree holds every collection's rows — model-independent, evolving only when the
    /// backend itself does — keyed by a surrogate id: each node is one address level
    /// under its parent node, rooted at the self-referential sentinel `id = 0`
    /// (`factory::ensure` seeds it), so `parent_id` is `NOT NULL` everywhere. It is
    /// the sole durable row representation; reads are served directly from it by
    /// indexed SQL statements (`DESIGN-pure-pg.md` §4), with no in-memory projection.
    /// The self-FK is `DEFERRABLE INITIALLY DEFERRED` so a parent-first insert within
    /// one transaction is tolerated.
    ///
    /// A node is a structural *position*; a *row* is a node carrying a value.
    /// `value`/`incarnation` are therefore NULLABLE: a live row has both non-NULL,
    /// while a **tombstone** — a deleted non-leaf ancestor retained so its descendant
    /// rows (logical orphans, §5.4) stay addressable — has both NULL. The
    /// `CHECK ((value IS NULL) = (incarnation IS NULL))` makes the mixed state
    /// unrepresentable, so `value IS NOT NULL` alone distinguishes a row from a
    /// tombstone. Delete tombstones a node in place rather than cascading its subtree,
    /// so descendants are untouched; a re-insert at a tombstoned address revives the
    /// same node (`ON CONFLICT DO UPDATE`), re-parenting its retained descendants
    /// under the live row again. `key_enc` is the order-preserving lookup/scan key;
    /// `key_wire` is the canonical, decodable key a load reconstructs the address from.
    /// `created` (JSONB, a self-describing timestamp) is the row's recorded admission
    /// instant (§14.1 `$created`, §22.6): non-NULL for a live row, NULL for a
    /// tombstone and the root sentinel. An insert stamps it with the commit's `now`;
    /// an update leaves it; a rekey carries the source's — so it is recorded once and
    /// preserved, matching the reference store. `commit_log.created` records the same
    /// per-commit `now` (§22.5) so a log-fold replay reconstructs each inserted row's
    /// `$created` identically to the head-state read.
    ///
    /// # `commit_log`: a settled admission, then a position
    ///
    /// An admission writes its `commit_log` row with **no serial position** — `seq`
    /// is NULLABLE and starts NULL — because §22.1 makes history construction follow
    /// committed transitions rather than take part in write admission. The row is
    /// keyed by `xid`, the id of the transaction that admitted it
    /// (`pg_current_xact_id()`, a never-reused 64-bit epoch-extended id), which is
    /// both its identity and the order the history builder ([`crate::history`])
    /// positions it in. `seq` is `UNIQUE` so two positions can never collide, and the
    /// builder stamps it only once the admitting transaction can no longer be beaten
    /// by an older one still in flight. `history_points.seq` and `commit_log.seq`
    /// therefore both name *built* history.
    #[must_use]
    pub fn tables(&self) -> [TableSpec; 6] {
        [
            TableSpec {
                name: "schema_version",
                columns: "id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1), version INT NOT NULL",
            },
            TableSpec {
                name: "instance_meta",
                columns: "id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1), \
                          instance_id TEXT NOT NULL, \
                          definition_source TEXT, \
                          definition_id TEXT, \
                          composition JSONB",
            },
            TableSpec {
                name: "nodes",
                columns: "id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                          parent_id BIGINT NOT NULL REFERENCES {schema}.nodes(id) \
                          DEFERRABLE INITIALLY DEFERRED, \
                          step_name TEXT NOT NULL, \
                          key_enc BYTEA NOT NULL, \
                          key_wire JSONB NOT NULL, \
                          incarnation TEXT, \
                          value JSONB, \
                          created JSONB, \
                          CHECK ((value IS NULL) = (incarnation IS NULL))",
            },
            TableSpec {
                name: "commit_log",
                columns: "xid XID8 PRIMARY KEY DEFAULT pg_current_xact_id(), \
                          seq BIGINT UNIQUE, \
                          transaction_id TEXT, ops JSONB NOT NULL, \
                          created JSONB NOT NULL",
            },
            TableSpec {
                name: "history_points",
                columns: "lineage TEXT NOT NULL, point TEXT NOT NULL, seq BIGINT NOT NULL, \
                          PRIMARY KEY (lineage, point)",
            },
            TableSpec { name: "blobs", columns: "digest TEXT PRIMARY KEY, bytes BYTEA NOT NULL" },
        ]
    }

    /// The minimal DDL that materializes the schema and its `schema_version` stamp
    /// table, and nothing else. The reconciler runs this first so it can read the
    /// stamp — and refuse an incompatible one — *before* applying DDL that assumes
    /// the current column layout.
    #[must_use]
    pub(crate) fn version_ddl(&self) -> String {
        let [version, ..] = self.tables();
        format!("CREATE SCHEMA IF NOT EXISTS {};\n{}", self.quoted(), version.create_sql(self))
    }

    /// The DDL that (idempotently) creates every fixed table, sequence and derived
    /// index this schema owns, built from the same [`tables`](Schema::tables),
    /// [`sequences`](Schema::sequences) and [`indexes`](Schema::indexes) data the
    /// reconciler diffs against.
    ///
    /// Tables and indexes are emitted in [`LOCK_ORDER`], never in declaration order.
    /// That is a correctness requirement, not tidiness: this DDL runs in one
    /// transaction and each `CREATE INDEX IF NOT EXISTS` holds a `ShareLock` on its
    /// table to the end of it, so emitting them in any order that disagrees with the
    /// order an admission writes those same tables lets a reconciling opener and a
    /// concurrent admission deadlock.
    #[must_use]
    pub fn create_ddl(&self) -> String {
        let mut ddl = format!("CREATE SCHEMA IF NOT EXISTS {};\n", self.quoted());
        let mut tables = self.tables();
        tables.sort_by_key(|table| lock_rank(table.name()));
        for table in tables {
            ddl.push_str(&table.create_sql(self));
            ddl.push('\n');
        }
        for sequence in self.sequences() {
            ddl.push_str(&sequence.create_sql(self));
            ddl.push('\n');
        }
        let mut indexes = self.indexes();
        indexes.sort_by_key(|index| lock_rank(index.table()));
        for index in indexes {
            ddl.push_str(&index.create_sql(self));
            ddl.push('\n');
        }
        ddl
    }

    /// DDL dropping this schema and everything in it — the droppable-unit tear
    /// down a test uses at the end of a run.
    #[must_use]
    pub fn drop_ddl(&self) -> String {
        format!("DROP SCHEMA IF EXISTS {} CASCADE;", self.quoted())
    }

    /// Idempotent DDL dropping a stray secondary `index` by its live catalog name
    /// — the reconciler's tool for retiring an orphan index that has fallen out of
    /// the declared set (an in-model index is retired through
    /// [`IndexSpec::drop_sql`] instead). Quoting mirrors [`Schema::quoted`].
    #[must_use]
    pub(crate) fn drop_index_sql(&self, index: &str) -> String {
        format!("DROP INDEX IF EXISTS {}.{};", self.quoted(), quote(index))
    }

    /// Idempotent DDL dropping a stray `sequence` by its live catalog name — the
    /// reconciler's tool for retiring a sequence that has fallen out of the declared
    /// set. Identity-column sequences are intrinsic to their table and are excluded
    /// from the live set, so this never reaches one.
    #[must_use]
    pub(crate) fn drop_sequence_sql(&self, sequence: &str) -> String {
        format!("DROP SEQUENCE IF EXISTS {}.{};", self.quoted(), quote(sequence))
    }

    /// Idempotent DDL dropping a stray `table` by its live catalog name — a
    /// leftover from a prior backend layout — cascading its dependents. The
    /// reconciler never passes a fixed table here, so the six are never dropped.
    #[must_use]
    pub(crate) fn drop_table_sql(&self, table: &str) -> String {
        format!("DROP TABLE IF EXISTS {}.{} CASCADE;", self.quoted(), quote(table))
    }

    /// A probe returning whether an orphan `table` holds any row (`present`), so the
    /// reconciler can refuse to silently `CASCADE`-drop a populated legacy table (a
    /// pre-node `rows`, say) rather than destroying data. An empty orphan still drops.
    #[must_use]
    pub(crate) fn table_nonempty_sql(&self, table: &str) -> String {
        format!("SELECT EXISTS (SELECT 1 FROM {}.{}) AS present", self.quoted(), quote(table))
    }
}

/// Quote a bare SQL identifier. Table and index names here are ASCII literals
/// this crate controls, so this is defence in depth mirroring [`Schema::quoted`].
fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn sanitize(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if out.is_empty() {
        out.push('x');
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A tiny FNV-1a over the raw identity bytes — a stable, dependency-free way to
/// keep derived names collision-resistant after truncation. Not a security hash.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
