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
//! # Model-derived indexes
//!
//! The secondary indexes a schema needs are *derived* from its tables
//! ([`Schema::indexes`]) rather than baked into one opaque DDL blob, so the set is
//! enumerable. Opening creates every derived index idempotently
//! (`CREATE INDEX IF NOT EXISTS`), and because the set is data a later
//! reconciliation round can diff the live indexes against it and drop any the
//! active model no longer needs — no migration leaves orphaned structures behind.
//! Primary-key and unique indexes are intrinsic to their table declarations (they
//! vanish with the table) and so are not part of this derived set.

/// The schema version this build writes and understands. Opening a schema with a
/// higher stamp is refused rather than guessed at; opening an older one applies
/// the current DDL (idempotently) and bumps the stamp forward.
///
/// Bumped to 2 when the model-derived `rows_key_order` index was added.
pub const SCHEMA_VERSION: i32 = 2;

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

    /// Idempotent creation DDL, scoped to `schema`.
    #[must_use]
    pub fn create_sql(&self, schema: &Schema) -> String {
        format!(
            "CREATE INDEX IF NOT EXISTS {} ON {}.{} ({});",
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
    /// Primary-key indexes (`rows(addr_key)`, `commit_log(seq)`, `blobs(digest)`,
    /// `history_points(lineage, point)`) are intrinsic to the table declarations
    /// and are not listed here — they serve every point lookup and the seq-ordered
    /// log reads directly, and drop with their table.
    #[must_use]
    pub fn indexes(&self) -> Vec<IndexSpec> {
        vec![
            // `InstanceStore::scan` enumerates a collection's direct rows in Annex B
            // key order over a shared `addr_key` prefix. The primary key on
            // `addr_key` orders by the database's *default* collation, which is not
            // guaranteed to be byte order — so a prefix range walk could sort or, on
            // some locales, decline the index entirely. A `COLLATE "C"` index gives
            // the prefix range a deterministic, byte-ordered, index-served path on
            // every cluster, so the scan never degrades to a Seq Scan + Sort.
            IndexSpec { name: "rows_key_order", table: "rows", key: "addr_key COLLATE \"C\"" },
        ]
    }

    /// The DDL that (idempotently) creates every table and derived index this
    /// schema owns.
    #[must_use]
    pub fn create_ddl(&self) -> String {
        let s = self.quoted();
        let mut ddl = format!(
            "CREATE SCHEMA IF NOT EXISTS {s};\n\
             CREATE TABLE IF NOT EXISTS {s}.schema_version (\
                 id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1), version INT NOT NULL);\n\
             CREATE TABLE IF NOT EXISTS {s}.instance_meta (\
                 id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1), \
                 head BIGINT NOT NULL, \
                 next_incarnation BIGINT NOT NULL, \
                 instance_id TEXT NOT NULL, \
                 definition_source TEXT, \
                 definition_id TEXT, \
                 composition JSONB);\n\
             CREATE TABLE IF NOT EXISTS {s}.rows (\
                 addr_key TEXT PRIMARY KEY, \
                 incarnation TEXT NOT NULL, \
                 value JSONB NOT NULL);\n\
             CREATE TABLE IF NOT EXISTS {s}.commit_log (\
                 seq BIGINT PRIMARY KEY, \
                 transaction_id TEXT, \
                 ops JSONB NOT NULL);\n\
             CREATE TABLE IF NOT EXISTS {s}.history_points (\
                 lineage TEXT NOT NULL, point TEXT NOT NULL, seq BIGINT NOT NULL, \
                 PRIMARY KEY (lineage, point));\n\
             CREATE TABLE IF NOT EXISTS {s}.blobs (\
                 digest TEXT PRIMARY KEY, bytes BYTEA NOT NULL);\n"
        );
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
