//! Typed runtime configuration loaded from environment variables.

use std::{
    env,
    net::SocketAddr,
    num::{NonZeroU16, NonZeroU32, NonZeroUsize},
    str::FromStr,
    time::Duration,
};

use secrecy::SecretString;
use thiserror::Error;
use url::Url;

/// Fully validated settings for one unprivileged runtime process.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Process role for which these settings were loaded.
    pub(crate) runtime_profile: RuntimeProfile,
    /// Runtime safety policy.
    pub environment: RuntimeEnvironment,
    /// HTTP listener and middleware policy.
    pub server: ServerSettings,
    /// PostgreSQL pool configuration.
    pub database: DatabaseSettings,
    /// Silicon platform service adapters.
    pub integrations: IntegrationSettings,
    /// Domain input limits.
    pub limits: LimitSettings,
    /// Durable outbox processing policy.
    pub worker: WorkerSettings,
    /// Structured tracing filter.
    pub log_filter: String,
}

/// Minimal settings accepted by the privileged migration command.
#[derive(Clone, Debug)]
pub struct MigrationSettings {
    /// Runtime safety policy.
    pub environment: RuntimeEnvironment,
    /// Privileged migration database pool configuration.
    pub database: DatabaseSettings,
    /// Exact PostgreSQL role that must own every Commit schema object.
    pub schema_owner: String,
    /// Structured tracing filter.
    pub log_filter: String,
}

/// Deployment environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEnvironment {
    /// Developer workstation.
    Development,
    /// Automated test process.
    Test,
    /// Deployed production process.
    Production,
}

/// Process-specific configuration capability set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeProfile {
    /// HTTP API, IAM, and Briefcase capability set.
    Api,
    /// Outbox worker and Hook capability set.
    Worker,
}

/// Identity authentication implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationMode {
    /// Verify credentials online with Silicon IAM.
    Iam,
    /// Accept explicit identity headers in non-production tests only.
    TrustedHeaders,
}

/// Listener and request middleware policy.
#[derive(Clone, Debug)]
pub struct ServerSettings {
    /// Address on which the API listens.
    pub bind_addr: SocketAddr,
    /// Canonical externally visible API base URL.
    pub public_base_url: Url,
    /// Exact browser origins allowed to call the API; empty denies CORS.
    pub cors_allowed_origins: Vec<Url>,
    /// Maximum request processing duration.
    pub request_timeout: Duration,
    /// Maximum accepted request body size.
    pub max_body_bytes: usize,
    /// Maximum concurrent in-flight requests per replica.
    pub concurrency_limit: usize,
    /// Graceful shutdown deadline.
    pub shutdown_timeout: Duration,
}

/// PostgreSQL connection-pool policy.
#[derive(Clone, Debug)]
pub struct DatabaseSettings {
    /// PostgreSQL URL, never emitted through logs.
    pub url: SecretString,
    /// Maximum open connections per process.
    pub max_connections: NonZeroU32,
    /// Minimum idle connections per process.
    pub min_connections: u32,
    /// Pool acquisition deadline.
    pub acquire_timeout: Duration,
    /// Database statement deadline.
    pub statement_timeout: Duration,
}

/// External Silicon service configuration.
#[derive(Clone, Debug)]
pub struct IntegrationSettings {
    /// IAM authentication and directory settings.
    pub iam: IamSettings,
    /// Briefcase temporary-URL adapter settings.
    pub briefcase: BriefcaseSettings,
    /// Hook delivery settings.
    pub hook: HookSettings,
    /// Outbound connect deadline.
    pub connect_timeout: Duration,
    /// Outbound request deadline.
    pub request_timeout: Duration,
    /// Maximum accepted provider response body.
    pub max_response_bytes: usize,
}

/// Silicon IAM adapter settings.
#[derive(Clone, Debug)]
pub struct IamSettings {
    /// Authentication implementation.
    pub mode: AuthenticationMode,
    /// IAM API base URL.
    pub base_url: Url,
    /// Commit IAM application ID.
    pub app_id: Option<String>,
    /// Commit IAM application secret.
    pub app_secret: Option<SecretString>,
    /// OBO proof audience expected by Commit.
    pub audience: String,
    /// Optional service bearer used for directory membership reads.
    pub directory_token: Option<SecretString>,
}

/// Silicon Briefcase adapter settings.
#[derive(Clone, Debug)]
pub struct BriefcaseSettings {
    /// Briefcase API base URL.
    pub base_url: Url,
    /// HTTPS origins accepted for permanent attachment URLs.
    pub allowed_origins: Vec<Url>,
}

/// Silicon Hook delivery adapter settings.
#[derive(Clone, Debug)]
pub struct HookSettings {
    /// Internal Hook event-publication endpoint; Hook owns endpoint routing.
    pub publish_url: Option<Url>,
    /// IAM service credential scoped to the internal Hook audience.
    pub service_token: Option<SecretString>,
}

/// Defensive domain input limits.
#[derive(Clone, Debug)]
pub struct LimitSettings {
    /// Maximum Unicode scalar count for a title.
    pub title_chars: NonZeroUsize,
    /// Maximum Unicode scalar count for a project name.
    pub project_name_chars: NonZeroUsize,
    /// Maximum Unicode scalar count for a description.
    pub description_chars: NonZeroUsize,
    /// Maximum Unicode scalar count for a note.
    pub note_chars: NonZeroUsize,
    /// Maximum attachments on one todo.
    pub attachments_per_todo: NonZeroUsize,
    /// Maximum current participants on one project.
    pub participants_per_project: NonZeroUsize,
    /// Idempotency response retention.
    pub idempotency_ttl: Duration,
}

/// Outbox worker policy.
#[derive(Clone, Debug)]
pub struct WorkerSettings {
    /// Maximum events recovered or considered in one database batch.
    pub batch_size: NonZeroUsize,
    /// Maximum Hook requests executing concurrently in one worker replica.
    pub delivery_concurrency: NonZeroUsize,
    /// Delay between empty polls.
    pub poll_interval: Duration,
    /// Exclusive claim duration.
    pub lease_duration: Duration,
    /// Maximum delivery attempts before dead-lettering.
    pub max_attempts: NonZeroU16,
    /// Maximum retry delay.
    pub max_retry_delay: Duration,
    /// Delay between retention-maintenance passes.
    pub maintenance_interval: Duration,
    /// Maximum rows affected by each individual maintenance statement.
    pub maintenance_batch_size: NonZeroUsize,
    /// Delay before deleted todo content is purged.
    pub todo_tombstone_retention: Duration,
    /// Retention for audit events and todo activity metadata.
    pub audit_retention: Duration,
    /// Retention for delivered outbox rows.
    pub delivered_outbox_retention: Duration,
    /// Retention for dead-lettered outbox rows.
    pub dead_letter_outbox_retention: Duration,
}

/// Redacted configuration loading failure.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// A required environment variable is absent.
    #[error("required environment variable {0} is missing")]
    Missing(&'static str),
    /// A value is malformed or violates runtime policy.
    #[error("invalid environment variable {name}: {reason}")]
    Invalid {
        /// Variable name.
        name: &'static str,
        /// Redacted failure reason.
        reason: String,
    },
}

impl Settings {
    /// Loads and validates settings for the HTTP API process.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for missing, malformed, or unsafe API settings.
    pub fn from_env_for_api() -> Result<Self, SettingsError> {
        Self::load_from_env(RuntimeProfile::Api)
    }

    /// Loads and validates settings for the outbox worker process.
    ///
    /// IAM, Briefcase, authentication, and HTTP-listener environment variables
    /// are deliberately not read by this profile.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for missing, malformed, or unsafe worker
    /// settings.
    pub fn from_env_for_worker() -> Result<Self, SettingsError> {
        Self::load_from_env(RuntimeProfile::Worker)
    }

    fn load_from_env(runtime_profile: RuntimeProfile) -> Result<Self, SettingsError> {
        let environment_value = optional("COMMIT_ENVIRONMENT");
        let environment = environment_from_value(environment_value.as_deref())?;
        let database = DatabaseSettings::runtime(environment)?;
        let server = match runtime_profile {
            RuntimeProfile::Api => load_api_server_settings(environment)?,
            RuntimeProfile::Worker => inactive_server_settings()?,
        };
        let integrations = load_integrations(runtime_profile)?;
        validate_integrations(runtime_profile, environment, &integrations)?;

        let limits = LimitSettings {
            title_chars: parse_or("COMMIT_MAX_TITLE_CHARS", "500")?,
            project_name_chars: parse_or("COMMIT_MAX_PROJECT_NAME_CHARS", "200")?,
            description_chars: parse_or("COMMIT_MAX_DESCRIPTION_CHARS", "20000")?,
            note_chars: parse_or("COMMIT_MAX_NOTE_CHARS", "20000")?,
            attachments_per_todo: parse_or("COMMIT_MAX_ATTACHMENTS_PER_TODO", "20")?,
            participants_per_project: parse_or("COMMIT_MAX_PROJECT_PARTICIPANTS", "100")?,
            idempotency_ttl: duration_secs("COMMIT_IDEMPOTENCY_TTL_SECONDS", 86_400)?,
        };
        validate_domain_limit_caps(&limits)?;

        let worker = WorkerSettings {
            batch_size: parse_or("COMMIT_WORKER_BATCH_SIZE", "100")?,
            delivery_concurrency: parse_or("COMMIT_WORKER_DELIVERY_CONCURRENCY", "16")?,
            poll_interval: duration_millis("COMMIT_WORKER_POLL_INTERVAL_MS", 1_000)?,
            lease_duration: duration_secs("COMMIT_WORKER_LEASE_SECONDS", 30)?,
            max_attempts: parse_or("COMMIT_WORKER_MAX_ATTEMPTS", "12")?,
            max_retry_delay: duration_secs("COMMIT_WORKER_MAX_RETRY_SECONDS", 3_600)?,
            maintenance_interval: duration_secs("COMMIT_MAINTENANCE_INTERVAL_SECONDS", 3_600)?,
            maintenance_batch_size: parse_or("COMMIT_MAINTENANCE_BATCH_SIZE", "1000")?,
            todo_tombstone_retention: duration_secs(
                "COMMIT_TODO_TOMBSTONE_RETENTION_SECONDS",
                3_888_000,
            )?,
            audit_retention: duration_secs("COMMIT_AUDIT_RETENTION_SECONDS", 220_752_000)?,
            delivered_outbox_retention: duration_secs(
                "COMMIT_DELIVERED_OUTBOX_RETENTION_SECONDS",
                2_592_000,
            )?,
            dead_letter_outbox_retention: duration_secs(
                "COMMIT_DEAD_LETTER_OUTBOX_RETENTION_SECONDS",
                7_776_000,
            )?,
        };
        validate_worker_settings(&limits, &worker, integrations.request_timeout)?;

        Ok(Self {
            runtime_profile,
            environment,
            server,
            database,
            integrations,
            limits,
            worker,
            log_filter: value_or("COMMIT_LOG", "silicon_commit=info,tower_http=info"),
        })
    }
}

fn load_api_server_settings(
    environment: RuntimeEnvironment,
) -> Result<ServerSettings, SettingsError> {
    let settings = ServerSettings {
        bind_addr: parse_or("COMMIT_BIND_ADDR", "127.0.0.1:8080")?,
        public_base_url: parse_url_or("COMMIT_PUBLIC_BASE_URL", "http://127.0.0.1:8080/api/v1/")?,
        cors_allowed_origins: parse_optional_origins("COMMIT_CORS_ALLOWED_ORIGINS")?,
        request_timeout: duration_secs("COMMIT_REQUEST_TIMEOUT_SECONDS", 15)?,
        max_body_bytes: parse_or("COMMIT_MAX_BODY_BYTES", "1048576")?,
        concurrency_limit: parse_or("COMMIT_CONCURRENCY_LIMIT", "1024")?,
        shutdown_timeout: duration_secs("COMMIT_SHUTDOWN_TIMEOUT_SECONDS", 30)?,
    };
    validate_public_base_url(environment, &settings.public_base_url)?;
    for origin in &settings.cors_allowed_origins {
        if environment == RuntimeEnvironment::Production && origin.scheme() != "https" {
            return Err(invalid(
                "COMMIT_CORS_ALLOWED_ORIGINS",
                "production origins must use HTTPS",
            ));
        }
    }
    if settings.concurrency_limit == 0 {
        return Err(invalid("COMMIT_CONCURRENCY_LIMIT", "must be positive"));
    }
    if settings.max_body_bytes == 0 {
        return Err(invalid("COMMIT_MAX_BODY_BYTES", "must be positive"));
    }
    Ok(settings)
}

fn inactive_server_settings() -> Result<ServerSettings, SettingsError> {
    Ok(ServerSettings {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        public_base_url: parse_url_value(
            "COMMIT_PUBLIC_BASE_URL",
            "http://unused.invalid/api/v1/",
        )?,
        cors_allowed_origins: Vec::new(),
        request_timeout: Duration::from_secs(15),
        max_body_bytes: 1,
        concurrency_limit: 1,
        // Shutdown policy is process-wide even though workers do not expose
        // an HTTP listener.
        shutdown_timeout: duration_secs("COMMIT_SHUTDOWN_TIMEOUT_SECONDS", 30)?,
    })
}

fn load_integrations(
    runtime_profile: RuntimeProfile,
) -> Result<IntegrationSettings, SettingsError> {
    let (iam, briefcase, hook) = match runtime_profile {
        RuntimeProfile::Api => (
            IamSettings {
                mode: parse_or("COMMIT_AUTH_MODE", "iam")?,
                base_url: parse_url_or(
                    "COMMIT_IAM_BASE_URL",
                    "https://iam.teamofsilicons.com/api/v1/",
                )?,
                app_id: optional("COMMIT_IAM_APP_ID"),
                app_secret: optional_secret("COMMIT_IAM_APP_SECRET"),
                audience: value_or("COMMIT_IAM_AUDIENCE", "silicon-commit"),
                directory_token: optional_secret("COMMIT_IAM_DIRECTORY_TOKEN"),
            },
            BriefcaseSettings {
                base_url: parse_url_or(
                    "COMMIT_BRIEFCASE_BASE_URL",
                    "https://briefcase.teamofsilicons.com/api/v1/",
                )?,
                allowed_origins: parse_origins("COMMIT_BRIEFCASE_ALLOWED_ORIGINS")?,
            },
            HookSettings {
                publish_url: None,
                service_token: None,
            },
        ),
        RuntimeProfile::Worker => (
            IamSettings {
                mode: AuthenticationMode::Iam,
                base_url: parse_url_value("COMMIT_IAM_BASE_URL", "http://unused.invalid/")?,
                app_id: None,
                app_secret: None,
                audience: String::new(),
                directory_token: None,
            },
            BriefcaseSettings {
                base_url: parse_url_value("COMMIT_BRIEFCASE_BASE_URL", "http://unused.invalid/")?,
                allowed_origins: Vec::new(),
            },
            HookSettings {
                publish_url: optional("COMMIT_HOOK_PUBLISH_URL")
                    .map(|value| {
                        Url::parse(&value)
                            .map_err(|error| invalid("COMMIT_HOOK_PUBLISH_URL", error.to_string()))
                    })
                    .transpose()?,
                service_token: optional_secret("COMMIT_HOOK_SERVICE_TOKEN"),
            },
        ),
    };

    Ok(IntegrationSettings {
        iam,
        briefcase,
        hook,
        connect_timeout: duration_millis("COMMIT_PROVIDER_CONNECT_TIMEOUT_MS", 1_000)?,
        request_timeout: duration_secs("COMMIT_PROVIDER_TIMEOUT_SECONDS", 5)?,
        max_response_bytes: parse_or("COMMIT_PROVIDER_MAX_RESPONSE_BYTES", "1048576")?,
    })
}

fn validate_domain_limit_caps(limits: &LimitSettings) -> Result<(), SettingsError> {
    const PUBLIC_CONTRACT_CAPS: [(&str, usize); 6] = [
        ("COMMIT_MAX_TITLE_CHARS", 500),
        ("COMMIT_MAX_PROJECT_NAME_CHARS", 200),
        ("COMMIT_MAX_DESCRIPTION_CHARS", 20_000),
        ("COMMIT_MAX_NOTE_CHARS", 20_000),
        ("COMMIT_MAX_ATTACHMENTS_PER_TODO", 20),
        ("COMMIT_MAX_PROJECT_PARTICIPANTS", 100),
    ];
    let configured = [
        limits.title_chars,
        limits.project_name_chars,
        limits.description_chars,
        limits.note_chars,
        limits.attachments_per_todo,
        limits.participants_per_project,
    ];

    for ((name, maximum), actual) in PUBLIC_CONTRACT_CAPS.into_iter().zip(configured) {
        if actual.get() > maximum {
            return Err(invalid(
                name,
                format!("must not exceed the public API limit of {maximum}"),
            ));
        }
    }
    Ok(())
}

fn validate_worker_settings(
    limits: &LimitSettings,
    worker: &WorkerSettings,
    provider_request_timeout: Duration,
) -> Result<(), SettingsError> {
    const MAX_DATABASE_BATCH_SIZE: usize = 10_000;
    const MAX_DELIVERY_CONCURRENCY: usize = 256;
    const MIN_IDEMPOTENCY_TTL: Duration = Duration::from_hours(24);
    const MIN_DELIVERED_OUTBOX_RETENTION: Duration = Duration::from_hours(720);
    const MIN_DEAD_LETTER_OUTBOX_RETENTION: Duration = Duration::from_hours(2_160);

    if limits.idempotency_ttl < MIN_IDEMPOTENCY_TTL {
        return Err(invalid(
            "COMMIT_IDEMPOTENCY_TTL_SECONDS",
            "must preserve the public 24-hour replay guarantee",
        ));
    }
    if limits.idempotency_ttl > worker.todo_tombstone_retention {
        return Err(invalid(
            "COMMIT_IDEMPOTENCY_TTL_SECONDS",
            "must not exceed COMMIT_TODO_TOMBSTONE_RETENTION_SECONDS",
        ));
    }
    if worker.delivered_outbox_retention < MIN_DELIVERED_OUTBOX_RETENTION {
        return Err(invalid(
            "COMMIT_DELIVERED_OUTBOX_RETENTION_SECONDS",
            "must preserve the 30-day terminal-event evidence minimum",
        ));
    }
    if worker.dead_letter_outbox_retention < MIN_DEAD_LETTER_OUTBOX_RETENTION {
        return Err(invalid(
            "COMMIT_DEAD_LETTER_OUTBOX_RETENTION_SECONDS",
            "must preserve the 90-day failed-event evidence minimum",
        ));
    }
    if worker.lease_duration <= provider_request_timeout {
        return Err(invalid(
            "COMMIT_WORKER_LEASE_SECONDS",
            "must exceed COMMIT_PROVIDER_TIMEOUT_SECONDS",
        ));
    }
    for (name, quantity) in [
        (
            "COMMIT_IDEMPOTENCY_TTL_SECONDS",
            limits.idempotency_ttl.as_millis(),
        ),
        (
            "COMMIT_WORKER_LEASE_SECONDS",
            u128::from(worker.lease_duration.as_secs()),
        ),
        (
            "COMMIT_WORKER_MAX_RETRY_SECONDS",
            worker.max_retry_delay.as_millis(),
        ),
        (
            "COMMIT_TODO_TOMBSTONE_RETENTION_SECONDS",
            u128::from(worker.todo_tombstone_retention.as_secs()),
        ),
        (
            "COMMIT_AUDIT_RETENTION_SECONDS",
            u128::from(worker.audit_retention.as_secs()),
        ),
        (
            "COMMIT_DELIVERED_OUTBOX_RETENTION_SECONDS",
            u128::from(worker.delivered_outbox_retention.as_secs()),
        ),
        (
            "COMMIT_DEAD_LETTER_OUTBOX_RETENTION_SECONDS",
            u128::from(worker.dead_letter_outbox_retention.as_secs()),
        ),
    ] {
        validate_i64_quantity(name, quantity)?;
    }
    for (name, batch_size) in [
        ("COMMIT_WORKER_BATCH_SIZE", worker.batch_size),
        (
            "COMMIT_MAINTENANCE_BATCH_SIZE",
            worker.maintenance_batch_size,
        ),
    ] {
        if batch_size.get() > MAX_DATABASE_BATCH_SIZE {
            return Err(invalid(
                name,
                format!("must not exceed {MAX_DATABASE_BATCH_SIZE}"),
            ));
        }
    }
    if worker.delivery_concurrency.get() > MAX_DELIVERY_CONCURRENCY {
        return Err(invalid(
            "COMMIT_WORKER_DELIVERY_CONCURRENCY",
            format!("must not exceed {MAX_DELIVERY_CONCURRENCY}"),
        ));
    }
    Ok(())
}

fn validate_i64_quantity(name: &'static str, quantity: u128) -> Result<(), SettingsError> {
    if quantity > i64::MAX as u128 {
        return Err(invalid(name, "is too large for PostgreSQL"));
    }
    Ok(())
}

impl LimitSettings {
    /// Converts runtime limits into the pure domain validation policy.
    #[must_use]
    pub fn domain_limits(&self) -> crate::domain::DomainLimits {
        crate::domain::DomainLimits {
            title_chars: self.title_chars.get(),
            project_name_chars: self.project_name_chars.get(),
            description_chars: self.description_chars.get(),
            note_chars: self.note_chars.get(),
            attachments_per_todo: self.attachments_per_todo.get(),
            participants_per_project: self.participants_per_project.get(),
        }
    }
}

impl MigrationSettings {
    /// Loads only configuration required by the migration binary.
    ///
    /// # Errors
    ///
    /// Returns a redacted error for missing, malformed, or unsafe settings.
    pub fn from_env() -> Result<Self, SettingsError> {
        let environment_value = optional("COMMIT_ENVIRONMENT");
        let environment = environment_from_value(environment_value.as_deref())?;
        let url = required("COMMIT_MIGRATOR_DATABASE_URL")?;
        let schema_owner = required("COMMIT_SCHEMA_OWNER")?;
        validate_database_transport(environment, &url, "COMMIT_MIGRATOR_DATABASE_URL")?;
        let database = DatabaseSettings {
            url: SecretString::from(url),
            max_connections: parse_or("COMMIT_MIGRATOR_DATABASE_MAX_CONNECTIONS", "2")?,
            min_connections: 0,
            acquire_timeout: duration_secs("COMMIT_DATABASE_ACQUIRE_TIMEOUT_SECONDS", 3)?,
            statement_timeout: duration_secs("COMMIT_MIGRATION_STATEMENT_TIMEOUT_SECONDS", 300)?,
        };
        validate_i64_quantity(
            "COMMIT_MIGRATION_STATEMENT_TIMEOUT_SECONDS",
            database.statement_timeout.as_millis(),
        )?;
        Ok(Self {
            environment,
            database,
            schema_owner,
            log_filter: value_or("COMMIT_LOG", "silicon_commit=info"),
        })
    }
}

impl DatabaseSettings {
    fn runtime(environment: RuntimeEnvironment) -> Result<Self, SettingsError> {
        let url = required("COMMIT_DATABASE_URL")?;
        validate_database_transport(environment, &url, "COMMIT_DATABASE_URL")?;
        let settings = Self {
            url: SecretString::from(url),
            max_connections: parse_or("COMMIT_DATABASE_MAX_CONNECTIONS", "16")?,
            min_connections: parse_or("COMMIT_DATABASE_MIN_CONNECTIONS", "1")?,
            acquire_timeout: duration_secs("COMMIT_DATABASE_ACQUIRE_TIMEOUT_SECONDS", 3)?,
            statement_timeout: duration_secs("COMMIT_DATABASE_STATEMENT_TIMEOUT_SECONDS", 10)?,
        };
        if settings.min_connections >= settings.max_connections.get() {
            return Err(invalid(
                "COMMIT_DATABASE_MIN_CONNECTIONS",
                "must be lower than COMMIT_DATABASE_MAX_CONNECTIONS",
            ));
        }
        validate_i64_quantity(
            "COMMIT_DATABASE_STATEMENT_TIMEOUT_SECONDS",
            settings.statement_timeout.as_millis(),
        )?;
        Ok(settings)
    }
}

fn validate_integrations(
    runtime_profile: RuntimeProfile,
    environment: RuntimeEnvironment,
    integrations: &IntegrationSettings,
) -> Result<(), SettingsError> {
    if integrations.max_response_bytes == 0 {
        return Err(invalid(
            "COMMIT_PROVIDER_MAX_RESPONSE_BYTES",
            "must be positive",
        ));
    }

    match runtime_profile {
        RuntimeProfile::Api => validate_api_integrations(environment, integrations),
        RuntimeProfile::Worker => validate_worker_integrations(environment, integrations),
    }
}

fn validate_api_integrations(
    environment: RuntimeEnvironment,
    integrations: &IntegrationSettings,
) -> Result<(), SettingsError> {
    if environment == RuntimeEnvironment::Production
        && integrations.iam.mode == AuthenticationMode::TrustedHeaders
    {
        return Err(invalid(
            "COMMIT_AUTH_MODE",
            "trusted_headers is forbidden in production",
        ));
    }
    if integrations.iam.mode == AuthenticationMode::Iam {
        for (name, configured) in [
            ("COMMIT_IAM_APP_ID", integrations.iam.app_id.is_some()),
            (
                "COMMIT_IAM_APP_SECRET",
                integrations.iam.app_secret.is_some(),
            ),
            (
                "COMMIT_IAM_DIRECTORY_TOKEN",
                integrations.iam.directory_token.is_some(),
            ),
        ] {
            if !configured {
                return Err(SettingsError::Missing(name));
            }
        }
    }
    validate_http_url(
        environment,
        &integrations.iam.base_url,
        "COMMIT_IAM_BASE_URL",
    )?;
    validate_http_url(
        environment,
        &integrations.briefcase.base_url,
        "COMMIT_BRIEFCASE_BASE_URL",
    )?;
    if integrations.briefcase.allowed_origins.is_empty() {
        return Err(invalid(
            "COMMIT_BRIEFCASE_ALLOWED_ORIGINS",
            "must contain at least one origin",
        ));
    }
    for origin in &integrations.briefcase.allowed_origins {
        validate_http_url(environment, origin, "COMMIT_BRIEFCASE_ALLOWED_ORIGINS")?;
        if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
            return Err(invalid(
                "COMMIT_BRIEFCASE_ALLOWED_ORIGINS",
                "each value must be an origin without a path, query, or fragment",
            ));
        }
    }

    Ok(())
}

fn validate_worker_integrations(
    environment: RuntimeEnvironment,
    integrations: &IntegrationSettings,
) -> Result<(), SettingsError> {
    match (
        &integrations.hook.publish_url,
        &integrations.hook.service_token,
    ) {
        (Some(url), Some(_)) => {
            validate_http_url(environment, url, "COMMIT_HOOK_PUBLISH_URL")?;
        }
        (None, None) if environment != RuntimeEnvironment::Production => {}
        (None, None) => {
            return Err(SettingsError::Missing("COMMIT_HOOK_PUBLISH_URL"));
        }
        _ => {
            return Err(invalid(
                "COMMIT_HOOK_PUBLISH_URL",
                "publish URL and service token must be configured together",
            ));
        }
    }
    Ok(())
}

fn parse_origins(name: &'static str) -> Result<Vec<Url>, SettingsError> {
    let raw = value_or(name, "https://briefcase.teamofsilicons.com");
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Url::parse(value).map_err(|error| invalid(name, error.to_string())))
        .collect()
}

fn parse_optional_origins(name: &'static str) -> Result<Vec<Url>, SettingsError> {
    let Some(raw) = optional(name) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let url = Url::parse(value).map_err(|error| invalid(name, error.to_string()))?;
            if url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
                || !matches!(url.scheme(), "http" | "https")
            {
                return Err(invalid(name, "each value must be an HTTP(S) origin"));
            }
            Ok(url)
        })
        .collect()
}

fn validate_database_transport(
    environment: RuntimeEnvironment,
    value: &str,
    name: &'static str,
) -> Result<(), SettingsError> {
    let url = Url::parse(value).map_err(|error| invalid(name, error.to_string()))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err(invalid(name, "must use postgres:// or postgresql://"));
    }

    let mut ssl_mode = None;
    for (key, value) in url.query_pairs() {
        if matches!(key.as_ref(), "sslmode" | "ssl-mode")
            && ssl_mode
                .replace(value.as_ref().to_ascii_lowercase())
                .is_some()
        {
            return Err(invalid(
                name,
                "must contain at most one sslmode or ssl-mode parameter",
            ));
        }
    }
    if let Some(mode) = ssl_mode.as_deref()
        && !matches!(
            mode,
            "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full"
        )
    {
        return Err(invalid(name, "contains an unsupported TLS mode"));
    }
    if environment == RuntimeEnvironment::Production && ssl_mode.as_deref() != Some("verify-full") {
        return Err(invalid(
            name,
            "production connections require exactly one sslmode=verify-full or ssl-mode=verify-full parameter",
        ));
    }
    Ok(())
}

fn validate_public_base_url(
    environment: RuntimeEnvironment,
    url: &Url,
) -> Result<(), SettingsError> {
    const NAME: &str = "COMMIT_PUBLIC_BASE_URL";
    validate_http_url(environment, url, NAME)?;
    if url.path() != "/api/v1/" || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid(
            NAME,
            "must use the exact /api/v1/ path without a query or fragment",
        ));
    }
    Ok(())
}

fn validate_http_url(
    environment: RuntimeEnvironment,
    url: &Url,
    name: &'static str,
) -> Result<(), SettingsError> {
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid(name, "must be an absolute URL without credentials"));
    }
    if environment == RuntimeEnvironment::Production && url.scheme() != "https" {
        return Err(invalid(name, "production URLs must use HTTPS"));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(name, "must use HTTP or HTTPS"));
    }
    Ok(())
}

fn parse_url_or(name: &'static str, default: &str) -> Result<Url, SettingsError> {
    let value = value_or(name, default);
    parse_url_value(name, &value)
}

fn parse_url_value(name: &'static str, value: &str) -> Result<Url, SettingsError> {
    let mut url = Url::parse(value).map_err(|error| invalid(name, error.to_string()))?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn duration_secs(name: &'static str, default: u64) -> Result<Duration, SettingsError> {
    let seconds = parse_or(name, &default.to_string())?;
    if seconds == 0 {
        return Err(invalid(name, "must be positive"));
    }
    Ok(Duration::from_secs(seconds))
}

fn duration_millis(name: &'static str, default: u64) -> Result<Duration, SettingsError> {
    let milliseconds = parse_or(name, &default.to_string())?;
    if milliseconds == 0 {
        return Err(invalid(name, "must be positive"));
    }
    Ok(Duration::from_millis(milliseconds))
}

fn required(name: &'static str) -> Result<String, SettingsError> {
    optional(name).ok_or(SettingsError::Missing(name))
}

fn environment_from_value(value: Option<&str>) -> Result<RuntimeEnvironment, SettingsError> {
    value
        .ok_or(SettingsError::Missing("COMMIT_ENVIRONMENT"))?
        .parse()
        .map_err(|error: &'static str| invalid("COMMIT_ENVIRONMENT", error))
}

fn optional(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn optional_secret(name: &'static str) -> Option<SecretString> {
    optional(name).map(SecretString::from)
}

fn value_or(name: &'static str, default: &str) -> String {
    optional(name).unwrap_or_else(|| default.to_owned())
}

fn parse_or<T>(name: &'static str, default: &str) -> Result<T, SettingsError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value_or(name, default)
        .parse()
        .map_err(|error: T::Err| invalid(name, error.to_string()))
}

fn invalid(name: &'static str, reason: impl Into<String>) -> SettingsError {
    SettingsError::Invalid {
        name,
        reason: reason.into(),
    }
}

impl FromStr for RuntimeEnvironment {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" | "prod" => Ok(Self::Production),
            _ => Err("expected development, test, or production"),
        }
    }
}

impl FromStr for AuthenticationMode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "iam" => Ok(Self::Iam),
            "trusted_headers" => Ok(Self::TrustedHeaders),
            _ => Err("expected iam or trusted_headers"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, time::Duration};

    use secrecy::SecretString;
    use url::Url;

    use super::{
        AuthenticationMode, BriefcaseSettings, HookSettings, IamSettings, IntegrationSettings,
        LimitSettings, RuntimeEnvironment, RuntimeProfile, SettingsError, WorkerSettings,
        environment_from_value, parse_url_value, validate_database_transport,
        validate_domain_limit_caps, validate_i64_quantity, validate_integrations,
        validate_public_base_url, validate_worker_settings,
    };

    #[test]
    fn parses_closed_environment_values() {
        assert_eq!("prod".parse(), Ok(RuntimeEnvironment::Production));
        assert_eq!("test".parse(), Ok(RuntimeEnvironment::Test));
        assert!("staging".parse::<RuntimeEnvironment>().is_err());
        assert!(matches!(
            environment_from_value(None),
            Err(SettingsError::Missing("COMMIT_ENVIRONMENT"))
        ));
    }

    #[test]
    fn parses_closed_authentication_modes() {
        assert_eq!("iam".parse(), Ok(AuthenticationMode::Iam));
        assert_eq!(
            "trusted_headers".parse(),
            Ok(AuthenticationMode::TrustedHeaders)
        );
        assert!("disabled".parse::<AuthenticationMode>().is_err());
    }

    #[test]
    fn api_profile_validates_only_iam_and_briefcase_adapters() {
        let mut integrations = integration_settings();
        integrations.hook.publish_url = Some(url("http://insecure-hook.invalid/events"));
        integrations.hook.service_token = None;

        assert!(
            validate_integrations(
                RuntimeProfile::Api,
                RuntimeEnvironment::Production,
                &integrations,
            )
            .is_ok()
        );

        integrations.iam.app_secret = None;
        assert!(matches!(
            validate_integrations(
                RuntimeProfile::Api,
                RuntimeEnvironment::Production,
                &integrations,
            ),
            Err(SettingsError::Missing("COMMIT_IAM_APP_SECRET"))
        ));
    }

    #[test]
    fn worker_profile_validates_only_hook_adapter() {
        let mut integrations = integration_settings();
        integrations.iam.mode = AuthenticationMode::TrustedHeaders;
        integrations.iam.base_url = url("http://insecure-iam.invalid/");
        integrations.iam.app_id = None;
        integrations.iam.app_secret = None;
        integrations.iam.directory_token = None;
        integrations.briefcase.base_url = url("http://insecure-briefcase.invalid/");
        integrations.briefcase.allowed_origins.clear();

        assert!(
            validate_integrations(
                RuntimeProfile::Worker,
                RuntimeEnvironment::Production,
                &integrations,
            )
            .is_ok()
        );

        integrations.hook.publish_url = None;
        integrations.hook.service_token = None;
        assert!(matches!(
            validate_integrations(
                RuntimeProfile::Worker,
                RuntimeEnvironment::Production,
                &integrations,
            ),
            Err(SettingsError::Missing("COMMIT_HOOK_PUBLISH_URL"))
        ));
    }

    #[test]
    fn production_api_forbids_trusted_header_authentication() {
        let mut integrations = integration_settings();
        integrations.iam.mode = AuthenticationMode::TrustedHeaders;

        assert!(matches!(
            validate_integrations(
                RuntimeProfile::Api,
                RuntimeEnvironment::Production,
                &integrations,
            ),
            Err(SettingsError::Invalid {
                name: "COMMIT_AUTH_MODE",
                ..
            })
        ));
    }

    #[test]
    fn production_database_tls_mode_is_secure_and_unambiguous() {
        for query in ["sslmode=verify-full", "ssl-mode=VERIFY-FULL"] {
            let database_url = format!("postgres://commit@db/commit?{query}");
            assert!(
                validate_database_transport(
                    RuntimeEnvironment::Production,
                    &database_url,
                    "TEST_DATABASE_URL",
                )
                .is_ok(),
                "expected {query} to be accepted"
            );
        }

        for query in [
            "",
            "sslmode=disable",
            "sslmode=prefer",
            "sslmode=require",
            "sslmode=verify-ca",
            "sslmode=unknown",
            "sslmode=require&sslmode=disable",
            "sslmode=require&ssl-mode=disable",
            "ssl-mode=require&sslmode=verify-full",
        ] {
            let database_url = if query.is_empty() {
                "postgres://commit@db/commit".to_owned()
            } else {
                format!("postgres://commit@db/commit?{query}")
            };
            assert!(
                validate_database_transport(
                    RuntimeEnvironment::Production,
                    &database_url,
                    "TEST_DATABASE_URL",
                )
                .is_err(),
                "expected {query:?} to be rejected"
            );
        }
    }

    #[test]
    fn nonproduction_database_urls_still_reject_ambiguous_tls_modes() {
        assert!(
            validate_database_transport(
                RuntimeEnvironment::Development,
                "postgres://commit@db/commit",
                "TEST_DATABASE_URL",
            )
            .is_ok()
        );
        for query in [
            "sslmode=unknown",
            "sslmode=disable&sslmode=require",
            "sslmode=disable&ssl-mode=require",
        ] {
            let database_url = format!("postgres://commit@db/commit?{query}");
            assert!(
                validate_database_transport(
                    RuntimeEnvironment::Development,
                    &database_url,
                    "TEST_DATABASE_URL",
                )
                .is_err()
            );
        }
    }

    #[test]
    fn public_base_url_is_the_canonical_v1_mount() {
        let canonical = parse_url_value("COMMIT_PUBLIC_BASE_URL", "https://commit.example/api/v1");
        assert!(canonical.as_ref().is_ok_and(|url| {
            validate_public_base_url(RuntimeEnvironment::Production, url).is_ok()
        }));

        for value in [
            "https://commit.example/",
            "https://commit.example/prefix/api/v1/",
            "https://commit.example/api/v2/",
            "https://commit.example/api/v1/?source=bad",
        ] {
            let url = parse_url_value("COMMIT_PUBLIC_BASE_URL", value);
            assert!(url.as_ref().is_ok_and(|url| {
                validate_public_base_url(RuntimeEnvironment::Production, url).is_err()
            }));
        }
    }

    #[test]
    fn rejects_runtime_limits_above_public_contract_caps() {
        let mut limits = limit_settings();
        assert!(validate_domain_limit_caps(&limits).is_ok());

        for (name, value) in [
            ("COMMIT_MAX_TITLE_CHARS", 501),
            ("COMMIT_MAX_PROJECT_NAME_CHARS", 201),
            ("COMMIT_MAX_DESCRIPTION_CHARS", 20_001),
            ("COMMIT_MAX_NOTE_CHARS", 20_001),
            ("COMMIT_MAX_ATTACHMENTS_PER_TODO", 21),
            ("COMMIT_MAX_PROJECT_PARTICIPANTS", 101),
        ] {
            limits = limit_settings();
            match name {
                "COMMIT_MAX_TITLE_CHARS" => limits.title_chars = nonzero(value),
                "COMMIT_MAX_PROJECT_NAME_CHARS" => {
                    limits.project_name_chars = nonzero(value);
                }
                "COMMIT_MAX_DESCRIPTION_CHARS" => limits.description_chars = nonzero(value),
                "COMMIT_MAX_NOTE_CHARS" => limits.note_chars = nonzero(value),
                "COMMIT_MAX_ATTACHMENTS_PER_TODO" => {
                    limits.attachments_per_todo = nonzero(value);
                }
                "COMMIT_MAX_PROJECT_PARTICIPANTS" => {
                    limits.participants_per_project = nonzero(value);
                }
                _ => unreachable!("test table contains an unknown setting"),
            }
            assert!(matches!(
                validate_domain_limit_caps(&limits),
                Err(SettingsError::Invalid { name: actual, .. }) if actual == name
            ));
        }
    }

    #[test]
    fn constrains_idempotency_to_tombstone_retention() {
        let mut limits = limit_settings();
        let worker = worker_settings();
        limits.idempotency_ttl = worker.todo_tombstone_retention;
        assert!(validate_worker_settings(&limits, &worker, provider_timeout()).is_ok());

        limits.idempotency_ttl += Duration::from_secs(1);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_IDEMPOTENCY_TTL_SECONDS",
                ..
            })
        ));
        assert!("0".parse::<NonZeroUsize>().is_err());
    }

    #[test]
    fn preserves_the_public_idempotency_replay_window() {
        let mut limits = limit_settings();
        let worker = worker_settings();
        limits.idempotency_ttl = Duration::from_hours(24);
        assert!(validate_worker_settings(&limits, &worker, provider_timeout()).is_ok());

        limits.idempotency_ttl -= Duration::from_secs(1);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_IDEMPOTENCY_TTL_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn preserves_minimum_terminal_outbox_evidence_windows() {
        let limits = limit_settings();
        let mut worker = worker_settings();
        worker.delivered_outbox_retention = Duration::from_hours(720);
        worker.dead_letter_outbox_retention = Duration::from_hours(2_160);
        assert!(validate_worker_settings(&limits, &worker, provider_timeout()).is_ok());

        worker.delivered_outbox_retention -= Duration::from_secs(1);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_DELIVERED_OUTBOX_RETENTION_SECONDS",
                ..
            })
        ));

        worker = worker_settings();
        worker.dead_letter_outbox_retention -= Duration::from_secs(1);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_DEAD_LETTER_OUTBOX_RETENTION_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn bounds_worker_and_maintenance_batches() {
        let limits = limit_settings();
        let mut worker = worker_settings();

        worker.batch_size = nonzero(10_001);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_WORKER_BATCH_SIZE",
                ..
            })
        ));

        worker = worker_settings();
        worker.maintenance_batch_size = nonzero(10_001);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_MAINTENANCE_BATCH_SIZE",
                ..
            })
        ));

        worker = worker_settings();
        worker.delivery_concurrency = nonzero(257);
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_WORKER_DELIVERY_CONCURRENCY",
                ..
            })
        ));
    }

    #[test]
    fn lease_outlives_provider_request_and_database_quantities_fit_i64() {
        let limits = limit_settings();
        let mut worker = worker_settings();
        worker.lease_duration = provider_timeout();
        assert!(matches!(
            validate_worker_settings(&limits, &worker, provider_timeout()),
            Err(SettingsError::Invalid {
                name: "COMMIT_WORKER_LEASE_SECONDS",
                ..
            })
        ));

        assert!(validate_i64_quantity("TEST_DURATION", i64::MAX as u128).is_ok());
        assert!(matches!(
            validate_i64_quantity("TEST_DURATION", i64::MAX as u128 + 1),
            Err(SettingsError::Invalid {
                name: "TEST_DURATION",
                ..
            })
        ));
    }

    fn limit_settings() -> LimitSettings {
        LimitSettings {
            title_chars: nonzero(500),
            project_name_chars: nonzero(200),
            description_chars: nonzero(20_000),
            note_chars: nonzero(20_000),
            attachments_per_todo: nonzero(20),
            participants_per_project: nonzero(100),
            idempotency_ttl: Duration::from_hours(24),
        }
    }

    fn worker_settings() -> WorkerSettings {
        WorkerSettings {
            batch_size: nonzero(100),
            delivery_concurrency: nonzero(16),
            poll_interval: Duration::from_secs(1),
            lease_duration: Duration::from_secs(30),
            max_attempts: std::num::NonZeroU16::MIN,
            max_retry_delay: Duration::from_hours(1),
            maintenance_interval: Duration::from_hours(1),
            maintenance_batch_size: nonzero(1_000),
            todo_tombstone_retention: Duration::from_hours(1_080),
            audit_retention: Duration::from_hours(61_320),
            delivered_outbox_retention: Duration::from_hours(720),
            dead_letter_outbox_retention: Duration::from_hours(2_160),
        }
    }

    fn integration_settings() -> IntegrationSettings {
        IntegrationSettings {
            iam: IamSettings {
                mode: AuthenticationMode::Iam,
                base_url: url("https://iam.example.test/api/v1/"),
                app_id: Some("silicon-commit".to_owned()),
                app_secret: Some(SecretString::from("app-secret")),
                audience: "silicon-commit".to_owned(),
                directory_token: Some(SecretString::from("directory-token")),
            },
            briefcase: BriefcaseSettings {
                base_url: url("https://briefcase.example.test/api/v1/"),
                allowed_origins: vec![url("https://briefcase.example.test/")],
            },
            hook: HookSettings {
                publish_url: Some(url("https://hook.example.test/events")),
                service_token: Some(SecretString::from("hook-token")),
            },
            connect_timeout: Duration::from_secs(1),
            request_timeout: provider_timeout(),
            max_response_bytes: 1_048_576,
        }
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap_or_else(|error| panic!("test URL failed: {error}"))
    }

    fn nonzero(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
    }

    const fn provider_timeout() -> Duration {
        Duration::from_secs(5)
    }
}
