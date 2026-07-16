//! Shared test support for the liasse-pg integration suites: resolve a working
//! PostgreSQL DSN, keep the (optional) disposable cluster alive for the duration
//! of a test process, and drop throwaway schemas even when a test panics.
//!
//! # DSN resolution order
//!
//! [`acquire`] resolves a DSN once per test process, trying in order:
//!
//! 1. **`LIASSE_PG_TEST_DSN`** — an explicit override. If it is set but cannot be
//!    connected to, the tests fail loudly rather than falling through.
//! 2. **The default local unix-socket DSN** (`host=/var/run/postgresql
//!    dbname=postgres`) if it accepts a connection — a developer machine whose OS
//!    user has a role.
//! 3. **A disposable cluster bootstrapped on demand**: `initdb` into a unique
//!    temp directory, `pg_ctl start` on a *private* unix-socket directory with
//!    `trust` auth and no TCP listener, connected once it answers. The server
//!    binaries are found on `PATH` first, then under `/usr/lib/postgresql/17/bin`
//!    (the usual Ubuntu location), then any other installed major version. The
//!    cluster is bootstrapped once, shared by every test in the process, and torn
//!    down — postmaster stopped, temp directories removed — when the last test
//!    handle drops (including on a panicking unwind).
//!
//! If none of the three yields a connection, the tests fail with a single
//! actionable message instead of silently passing.
#![allow(dead_code, clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use liasse_ident::InstanceId;
use liasse_pg::PgStoreFactory;

/// The default DSN tried before bootstrapping a disposable cluster.
const DEFAULT_DSN: &str = "host=/var/run/postgresql dbname=postgres";

/// Process-wide shared test-database state, resolved lazily on first [`acquire`].
struct Shared {
    dsn: Option<String>,
    cluster: Option<Cluster>,
    active: usize,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { dsn: None, cluster: None, active: 0 });
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A live handle to the shared test PostgreSQL. Hold it for the whole of a test:
/// it keeps a bootstrapped cluster alive, and when the last outstanding handle in
/// the process drops — on success or on a panicking unwind — the cluster is shut
/// down and its files removed.
pub struct PgHandle {
    dsn: String,
}

impl PgHandle {
    /// A factory over a namespace unique to this call, so parallel tests in the
    /// same process never share a schema.
    #[must_use]
    pub fn factory(&self, seed: &str) -> PgStoreFactory {
        let namespace =
            format!("{seed}_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        PgStoreFactory::new(&self.dsn, namespace)
    }
}

impl Drop for PgHandle {
    fn drop(&mut self) {
        release();
    }
}

/// Drops `instance`'s schema when this guard falls out of scope — on the success
/// path and, crucially, when a failing assertion unwinds past it, which a
/// success-path-only teardown would leak. It owns its own factory clone, so it
/// needs no borrow of the one under test, and its drop is best-effort and never
/// panics.
pub struct SchemaGuard {
    factory: PgStoreFactory,
    instance: InstanceId,
}

impl SchemaGuard {
    #[must_use]
    pub fn new(factory: &PgStoreFactory, instance: InstanceId) -> Self {
        Self { factory: factory.clone(), instance }
    }
}

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        let _ = self.factory.drop_instance(&self.instance);
    }
}

/// Acquire a handle to the shared test PostgreSQL, resolving (and bootstrapping)
/// it on first use. Panics with an actionable message if no database can be
/// obtained — tests are allowed to panic, and a green run must actually exercise
/// PostgreSQL rather than skip.
#[must_use]
pub fn acquire() -> PgHandle {
    let mut shared = lock();
    let dsn = match &shared.dsn {
        Some(dsn) => dsn.clone(),
        None => {
            let (dsn, cluster) = resolve();
            shared.cluster = cluster;
            shared.dsn = Some(dsn.clone());
            dsn
        }
    };
    shared.active += 1;
    PgHandle { dsn }
}

fn release() {
    let cluster = {
        let mut shared = lock();
        shared.active = shared.active.saturating_sub(1);
        if shared.active == 0 {
            // Last handle out: forget the resolved DSN so a later acquire
            // re-resolves, and hand back any bootstrapped cluster to shut down.
            shared.dsn = None;
            shared.cluster.take()
        } else {
            None
        }
    };
    if let Some(cluster) = cluster {
        cluster.shutdown();
    }
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Resolve the DSN following the documented order, bootstrapping a disposable
/// cluster if needed. Panics with a single actionable message if all three
/// strategies fail.
fn resolve() -> (String, Option<Cluster>) {
    // (a) Explicit override.
    let override_dsn =
        std::env::var("LIASSE_PG_TEST_DSN").ok().map(|d| d.trim().to_owned()).filter(|d| !d.is_empty());
    if let Some(dsn) = override_dsn {
        if can_connect(&dsn) {
            return (dsn, None);
        }
        panic!(
            "LIASSE_PG_TEST_DSN is set to `{dsn}` but no connection could be opened.\n\
             Point it at a reachable PostgreSQL 17, or unset it to let the tests \
             bootstrap a disposable cluster."
        );
    }

    // (b) Default local socket.
    if can_connect(DEFAULT_DSN) {
        return (DEFAULT_DSN.to_owned(), None);
    }

    // (c) Disposable cluster.
    match Cluster::bootstrap() {
        Ok((dsn, cluster)) => (dsn, Some(cluster)),
        Err(error) => {
            let override_state = if std::env::var_os("LIASSE_PG_TEST_DSN").is_some() {
                "set but unreachable"
            } else {
                "unset"
            };
            panic!(
                "no PostgreSQL is available for the liasse-pg integration tests.\n\
                 Resolution order tried:\n  \
                 (a) $LIASSE_PG_TEST_DSN — {override_state}\n  \
                 (b) default `{DEFAULT_DSN}` — no connection\n  \
                 (c) disposable cluster — {error}\n\
                 Fix one of these: set LIASSE_PG_TEST_DSN to a reachable PostgreSQL 17, \
                 run a local server with a role for the current OS user, or install the \
                 PostgreSQL 17 server binaries (initdb/pg_ctl on PATH or \
                 /usr/lib/postgresql/17/bin)."
            );
        }
    }
}

fn can_connect(dsn: &str) -> bool {
    PgStoreFactory::new(dsn, "probe").connect().is_ok()
}

/// A disposable PostgreSQL cluster owned by one test process.
struct Cluster {
    pg_ctl: PathBuf,
    data_dir: PathBuf,
    socket_dir: PathBuf,
}

impl Cluster {
    /// `initdb` a fresh cluster, start it on a private unix socket with no TCP
    /// listener, and return its DSN once it accepts connections.
    fn bootstrap() -> Result<(String, Self), String> {
        let initdb = locate("initdb")
            .ok_or_else(|| "initdb not found on PATH or /usr/lib/postgresql/17/bin".to_owned())?;
        let pg_ctl = locate("pg_ctl")
            .ok_or_else(|| "pg_ctl not found on PATH or /usr/lib/postgresql/17/bin".to_owned())?;

        let nonce = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let base = std::env::temp_dir();
        let data_dir = base.join(format!("liasse-pg-data-{nonce}"));
        let socket_dir = base.join(format!("liasse-pg-sock-{nonce}"));
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;
        std::fs::create_dir_all(&socket_dir)
            .map_err(|e| format!("could not create {}: {e}", socket_dir.display()))?;

        let cluster = Self { pg_ctl, data_dir, socket_dir };

        // `initdb` with trust auth and a `postgres` superuser; `--no-sync` because
        // a throwaway cluster never needs to survive a crash.
        let data = cluster.data_dir.to_string_lossy().into_owned();
        let out = Command::new(&initdb)
            .args(["-D", &data, "-A", "trust", "-U", "postgres", "--no-sync"])
            .output()
            .map_err(|e| format!("initdb failed to launch: {e}"))?;
        if !out.status.success() {
            cluster.remove_dirs();
            return Err(format!(
                "initdb exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        // Start the postmaster on the private socket directory with TCP disabled.
        // `-l` redirects the server's own output to a log file (leaving it on our
        // captured pipe can wedge a no-wait start), and `-W` returns immediately;
        // we then poll the socket ourselves rather than trust `pg_ctl -w`.
        let sock = cluster.socket_dir.to_string_lossy().into_owned();
        let logfile = cluster.data_dir.join("server.log");
        let log = logfile.to_string_lossy().into_owned();
        let options = format!("-k {sock} -c listen_addresses=");
        let start = Command::new(&cluster.pg_ctl)
            .args(["-D", &data, "-o", &options, "-l", &log, "-W", "start"])
            .output();

        let dsn = format!("host={sock} user=postgres dbname=postgres");
        for _ in 0..100 {
            if can_connect(&dsn) {
                return Ok((dsn, cluster));
            }
            std::thread::sleep(Duration::from_millis(300));
        }

        // Never became reachable within the budget: tear down and report with the
        // pg_ctl outcome and the server log, which name the actual cause.
        let detail = match start {
            Ok(out) => format!(
                "pg_ctl start exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => format!("pg_ctl failed to launch: {e}"),
        };
        let server_log = std::fs::read_to_string(&logfile).unwrap_or_default();
        cluster.shutdown();
        Err(format!("{detail}; server log:\n{}", server_log.trim()))
    }

    /// Best-effort teardown: stop the postmaster and remove the temp directories.
    /// Never panics — it runs from a `Drop` and may run during an unwind.
    fn shutdown(&self) {
        let data = self.data_dir.to_string_lossy().into_owned();
        let _ = Command::new(&self.pg_ctl)
            .args(["-D", &data, "-m", "immediate", "-w", "-t", "20", "stop"])
            .output();
        self.remove_dirs();
    }

    fn remove_dirs(&self) {
        let _ = std::fs::remove_dir_all(&self.data_dir);
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

/// Locate a PostgreSQL server `binary`: probe `PATH` first, then the usual Ubuntu
/// `/usr/lib/postgresql/17/bin`, then any other installed major version.
fn locate(binary: &str) -> Option<PathBuf> {
    if Command::new(binary).arg("--version").output().is_ok() {
        return Some(PathBuf::from(binary));
    }
    let mut candidates = vec![PathBuf::from(format!("/usr/lib/postgresql/17/bin/{binary}"))];
    if let Ok(entries) = std::fs::read_dir("/usr/lib/postgresql") {
        for entry in entries.flatten() {
            candidates.push(entry.path().join("bin").join(binary));
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}
