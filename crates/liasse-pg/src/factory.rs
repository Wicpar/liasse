//! [`PgStoreFactory`]: opens connections and materializes per-instance schemas.
//!
//! A factory carries the DSN and a namespace token that isolates a family of
//! instances (a test run's unique suffix, a deployment's fixed prefix). Each
//! store gets its own connection — one writer per instance means one connection
//! is enough — pointed at the instance's own PostgreSQL schema.

use std::time::Duration;

use liasse_ident::InstanceId;
use liasse_store::{StoreError, StoreFactory};
use postgres::{Client, NoTls};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;

use crate::backend::{backend, refuse};
use crate::reconcile::reconcile;
use crate::schema::Schema;
use crate::store::{PgStore, ReadPool};

/// The boring default read-pool size (§5.3 `max_size = 4`): a handful of read
/// connections per instance, enough for the `&self` read path without a test
/// suite that opens many instances multiplying idle connections.
const DEFAULT_POOL_SIZE: u32 = 4;

/// Constructs [`PgStore`]s over one DSN and namespace.
#[derive(Debug, Clone)]
pub struct PgStoreFactory {
    dsn: String,
    namespace: String,
    /// The `max_size` each store's read pool (§5) is built with. Optional knob;
    /// defaults to [`DEFAULT_POOL_SIZE`].
    pool_size: u32,
}

impl PgStoreFactory {
    /// A factory over `dsn`, isolating instances under `namespace`, with the
    /// default read-pool size ([`DEFAULT_POOL_SIZE`]).
    #[must_use]
    pub fn new(dsn: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self { dsn: dsn.into(), namespace: namespace.into(), pool_size: DEFAULT_POOL_SIZE }
    }

    /// Override the per-store read-pool `max_size` (§5.3). The default is boring
    /// ([`DEFAULT_POOL_SIZE`]); a caller with a different read-concurrency profile
    /// tunes it here.
    #[must_use]
    pub fn with_pool_size(mut self, pool_size: u32) -> Self {
        self.pool_size = pool_size.max(1);
        self
    }

    /// Build the `&self` read pool (§5) against this factory's DSN. Called by
    /// [`Self::create`]/[`Self::reopen`] *after* [`Self::ensure`] reconciles, so a
    /// pooled connection can only observe the reconciled schema (§5.3). The pool
    /// is lazy (`min_idle = 0`), so building it opens no connection — the writer's
    /// own reconcile already proved reachability; a pooled read connects on first
    /// checkout (Phase 1). A dead connection surfaces as a query error later, not
    /// a build failure (§5.3 fail-loud).
    fn build_read_pool(&self) -> Result<ReadPool, StoreError> {
        let config: postgres::Config = self.dsn.parse().map_err(backend)?;
        let manager = PostgresConnectionManager::new(config, NoTls);
        Pool::builder()
            .max_size(self.pool_size)
            .min_idle(Some(0))
            .connection_timeout(Duration::from_secs(5))
            .test_on_check_out(false)
            .build(manager)
            .map_err(|error| refuse(format!("read pool build failed: {error}")))
    }

    /// Open a fresh connection, and connect only — used by the test harness to
    /// report an actionable failure before any schema work.
    pub fn connect(&self) -> Result<Client, StoreError> {
        Client::connect(&self.dsn, NoTls).map_err(backend)
    }

    /// Reopen the existing schema for `instance` without wiping it — the durable
    /// path a process restart takes. Fails if the schema is missing or stamped
    /// newer than this build understands.
    pub fn reopen(&self, instance: InstanceId) -> Result<PgStore, StoreError> {
        let schema = Schema::derive(&self.namespace, instance.as_str());
        let mut client = self.connect()?;
        Self::ensure(&mut client, &schema, &instance)?;
        let reads = self.build_read_pool()?;
        PgStore::open(client, schema, instance, reads)
    }

    /// Drop `instance`'s schema and everything in it — the droppable-unit
    /// teardown an integration run performs when it is done.
    pub fn drop_instance(&self, instance: &InstanceId) -> Result<(), StoreError> {
        let schema = Schema::derive(&self.namespace, instance.as_str());
        let mut client = self.connect()?;
        client.batch_execute(&schema.drop_ddl()).map_err(backend)
    }

    /// The physical schema this factory materializes for `instance`. Exposed so
    /// operational tooling — and the index-coverage gates — can inspect the exact
    /// tables and derived indexes an instance owns.
    #[must_use]
    pub fn schema_for(&self, instance: &InstanceId) -> Schema {
        Schema::derive(&self.namespace, instance.as_str())
    }

    /// Reconcile the instance's physical schema against the current model — create
    /// what is missing, drop every orphan (see [`crate::reconcile`]) — then seed the
    /// single `instance_meta` row a fresh schema needs. Reconciliation stamps the
    /// version and refuses a schema newer than this build.
    fn ensure(client: &mut Client, schema: &Schema, instance: &InstanceId) -> Result<(), StoreError> {
        reconcile(client, schema)?;
        // An `InstanceId` is an unvalidated opaque token (D.1); NUL-safe-encode it so
        // a `U+0000` does not break the seed INSERT into the `text` column. The
        // column is informational (the live instance identity is passed to `open`,
        // never read back from here), so it needs no matching decode.
        let instance_id = crate::jsonb_text::encode_text(instance.as_str());
        client
            .execute(
                &format!(
                    "INSERT INTO {}.instance_meta (id, head, next_incarnation, instance_id) \
                     VALUES (1, 0, 0, $1) ON CONFLICT (id) DO NOTHING",
                    schema.quoted()
                ),
                &[&instance_id],
            )
            .map_err(backend)?;
        // Seed the self-referential root sentinel node (id = 0, parent_id = 0) that
        // every depth-1 row node hangs under, so `parent_id` is NOT NULL everywhere
        // and a load can stop the parent-walk at it. `OVERRIDING SYSTEM VALUE`
        // supplies the id = 0 the `GENERATED ALWAYS AS IDENTITY` column would refuse
        // (on PG17 the identity sequence still starts at 1 for real inserts); the
        // `ON CONFLICT (id) DO NOTHING` makes the seed idempotent across reopens.
        client
            .execute(
                &format!(
                    "INSERT INTO {}.nodes \
                     (id, parent_id, step_name, key_enc, key_wire, incarnation, value) \
                     OVERRIDING SYSTEM VALUE \
                     VALUES (0, 0, '', '\\x'::bytea, '{{}}'::jsonb, '', '{{}}') \
                     ON CONFLICT (id) DO NOTHING",
                    schema.quoted()
                ),
                &[],
            )
            .map_err(backend)?;
        Ok(())
    }
}

impl StoreFactory for PgStoreFactory {
    type Store = PgStore;

    /// Create a fresh, empty instance store at genesis. The instance's schema is
    /// dropped and recreated, so an instance identity is a clean slate every
    /// time (which is what the shared conformance battery expects, reusing one
    /// identity across cases).
    fn create(&mut self, instance: InstanceId) -> Result<Self::Store, StoreError> {
        let schema = Schema::derive(&self.namespace, instance.as_str());
        let mut client = self.connect()?;
        client.batch_execute(&schema.drop_ddl()).map_err(backend)?;
        Self::ensure(&mut client, &schema, &instance)?;
        let reads = self.build_read_pool()?;
        PgStore::open(client, schema, instance, reads)
    }
}
