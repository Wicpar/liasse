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

/// The schema version this build writes and understands. Opening a schema with a
/// higher stamp is refused rather than guessed at.
pub const SCHEMA_VERSION: i32 = 1;

/// A per-instance schema namespace: a validated PostgreSQL identifier.
#[derive(Debug, Clone)]
pub struct Schema {
    name: String,
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

    /// The DDL that (idempotently) creates every table this schema owns.
    #[must_use]
    pub fn create_ddl(&self) -> String {
        let s = self.quoted();
        format!(
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
        )
    }

    /// DDL dropping this schema and everything in it — the droppable-unit tear
    /// down a test uses at the end of a run.
    #[must_use]
    pub fn drop_ddl(&self) -> String {
        format!("DROP SCHEMA IF EXISTS {} CASCADE;", self.quoted())
    }
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
