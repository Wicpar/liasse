//! [`PgStoreFactory`]: opens connections and materializes per-instance schemas.
//!
//! A factory carries the DSN and a namespace token that isolates a family of
//! instances (a test run's unique suffix, a deployment's fixed prefix). Each
//! store gets its own connection — one writer per instance means one connection
//! is enough — pointed at the instance's own PostgreSQL schema.

use liasse_ident::InstanceId;
use liasse_store::{StoreError, StoreFactory};
use postgres::{Client, NoTls};

use crate::backend::backend;
use crate::reconcile::reconcile;
use crate::schema::Schema;
use crate::store::PgStore;

/// Constructs [`PgStore`]s over one DSN and namespace.
#[derive(Debug, Clone)]
pub struct PgStoreFactory {
    dsn: String,
    namespace: String,
}

impl PgStoreFactory {
    /// A factory over `dsn`, isolating instances under `namespace`.
    #[must_use]
    pub fn new(dsn: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self { dsn: dsn.into(), namespace: namespace.into() }
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
        PgStore::open(client, schema, instance)
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
        client
            .execute(
                &format!(
                    "INSERT INTO {}.instance_meta (id, head, next_incarnation, instance_id) \
                     VALUES (1, 0, 0, $1) ON CONFLICT (id) DO NOTHING",
                    schema.quoted()
                ),
                &[&instance.as_str()],
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
        PgStore::open(client, schema, instance)
    }
}
