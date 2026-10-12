use crate::config::Config;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// The connection pool, plus the embedded server when one was started.
///
/// The embedded handle is not dead weight: dropping a `PostgreSQL` stops the
/// server it manages, so it has to outlive the pool. It is held for the
/// lifetime of the process and stopped in [`Database::shutdown`].
pub struct Database {
    pub pool: PgPool,
    #[cfg(feature = "embedded-db")]
    embedded: Option<postgresql_embedded::PostgreSQL>,
}

/// Connect to Postgres.
///
/// When `DATABASE_URL` is set, that server is used and nothing else happens —
/// existing deployments are untouched.
///
/// When it is absent and the `embedded-db` feature is compiled in, SPACE starts
/// its own PostgreSQL from binaries bundled into this executable, so a fresh
/// machine needs nothing installed. Without that feature, the variable is
/// required and its absence is a startup error.
pub async fn connect(cfg: &Config) -> anyhow::Result<Database> {
    match cfg.database_url.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(url) => {
            tracing::info!("connecting to configured database");
            Ok(Database {
                pool: pool_for(url, cfg).await?,
                #[cfg(feature = "embedded-db")]
                embedded: None,
            })
        }
        None => start_embedded(cfg).await,
    }
}

async fn pool_for(url: &str, cfg: &Config) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.db_max_connections)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await?;
    Ok(pool)
}

/// Database created inside the embedded server.
#[cfg(feature = "embedded-db")]
const EMBEDDED_DB_NAME: &str = "space";

/// Bring up the bundled server against a persistent data directory.
#[cfg(feature = "embedded-db")]
async fn start_embedded(cfg: &Config) -> anyhow::Result<Database> {
    use postgresql_embedded::{PostgreSQL, Settings};

    let root = cfg
        .embedded_db_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("data").join("postgres"));
    std::fs::create_dir_all(&root)?;

    let mut settings = Settings::default();
    // The default is a throwaway instance whose data directory is deleted on
    // drop. This database is the product's storage, so it must persist.
    settings.temporary = false;
    settings.data_dir = root.join("data");
    settings.installation_dir = root.join("server");
    // Port 0 asks the OS for a free port, so this never fights with an already
    // installed Postgres, or with a second checkout on the same machine.
    settings.port = 0;

    tracing::info!(
        path = %root.display(),
        "no DATABASE_URL set; starting the bundled postgres server"
    );

    let mut postgresql = PostgreSQL::new(settings);
    // First run extracts the server and runs initdb, which takes a few
    // seconds. Every later start is fast.
    postgresql.setup().await?;
    postgresql.start().await?;

    if !postgresql.database_exists(EMBEDDED_DB_NAME).await? {
        postgresql.create_database(EMBEDDED_DB_NAME).await?;
    }

    let url = postgresql.settings().url(EMBEDDED_DB_NAME);
    let pool = pool_for(&url, cfg).await?;

    tracing::info!(port = postgresql.settings().port, "bundled postgres ready");

    Ok(Database {
        pool,
        embedded: Some(postgresql),
    })
}

#[cfg(not(feature = "embedded-db"))]
async fn start_embedded(_cfg: &Config) -> anyhow::Result<Database> {
    anyhow::bail!(
        "DATABASE_URL is not set. Either set it, or rebuild with \
         `--features embedded-db` to run the bundled PostgreSQL server \
         (needs the MSVC toolchain on Windows)."
    )
}

impl Database {
    /// Stop the embedded server, if this process started one.
    ///
    /// Called on shutdown so the data directory is left cleanly checkpointed
    /// rather than recovered on next boot.
    pub async fn shutdown(&mut self) {
        #[cfg(feature = "embedded-db")]
        if let Some(pg) = self.embedded.as_mut() {
            match pg.stop().await {
                Ok(()) => tracing::info!("bundled postgres stopped"),
                Err(e) => tracing::warn!(error = %e, "bundled postgres did not stop cleanly"),
            }
        }
    }
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let migrator = sqlx::migrate!("./migrations");
    let _report = migrator.run(pool).await?;
    apply_optional_layer(pool).await;
    tracing::info!("database migrations applied");
    Ok(())
}

/// Dialect-portability layer: PostgreSQL-specific extensions are applied
/// statement-by-statement; failures (e.g. CockroachDB without pg_trgm) are
/// logged once and skipped. Runtime queries degrade gracefully — see
/// `search::repo::full_text` for the ILIKE fallback path.
async fn apply_optional_layer(pool: &PgPool) {
    const OPTIONAL_PG_SEARCH: &str = include_str!("../migrations-optional/pg_search.sql");

    for statement in OPTIONAL_PG_SEARCH.split(';') {
        let stmt = statement.trim();
        if stmt.is_empty() || stmt.starts_with("--") && !stmt.contains('\n') {
            continue;
        }
        match sqlx::query(stmt).execute(pool).await {
            Ok(_) => {}
            Err(err) => tracing::info!(
                error = %err,
                "optional postgres extension unavailable; using fallbacks"
            ),
        }
    }
}
