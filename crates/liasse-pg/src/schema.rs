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
//! Both the fixed tables ([`Schema::tables`]) and the secondary indexes a schema
//! needs ([`Schema::indexes`]) are held as *data* rather than baked into one
//! opaque DDL blob, so each set is enumerable. Opening creates every table and
//! index idempotently (`CREATE … IF NOT EXISTS`), and because the sets are data a
//! later reconciliation round (see [`crate::reconcile`]) can diff the live objects
//! against them and drop any orphan the active model no longer declares — no
//! migration leaves orphaned structures behind. Primary-key indexes and `UNIQUE`
//! *table constraints* are intrinsic to their table declarations (they vanish with
//! the table) and so are not part of the derived index set; a bare
//! `CREATE UNIQUE INDEX` — like the node lookup — is a managed secondary index and
//! is in the set.

/// The schema version this build writes and understands. Opening a schema with a
/// higher stamp is refused rather than guessed at; opening an older one applies
/// the current DDL (idempotently) and bumps the stamp forward.
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
/// `$from` reads the instant a row was admitted.
pub const SCHEMA_VERSION: i32 = 5;

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
        format!(
            "CREATE {unique}INDEX IF NOT EXISTS {} ON {}.{} ({});",
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
            },
        ]
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
                          head BIGINT NOT NULL, \
                          next_incarnation BIGINT NOT NULL, \
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
                columns: "seq BIGINT PRIMARY KEY, transaction_id TEXT, ops JSONB NOT NULL, \
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

    /// The DDL that (idempotently) creates every fixed table and derived index
    /// this schema owns, built from the same [`tables`](Schema::tables) and
    /// [`indexes`](Schema::indexes) data the reconciler diffs against.
    #[must_use]
    pub fn create_ddl(&self) -> String {
        let mut ddl = format!("CREATE SCHEMA IF NOT EXISTS {};\n", self.quoted());
        for table in self.tables() {
            ddl.push_str(&table.create_sql(self));
            ddl.push('\n');
        }
        for index in self.indexes() {
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
