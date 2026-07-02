use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use newapi_usage_exporter::{
    cleanup_exports, create_export, export_request_from_instruction, export_zip_path, parse_time,
    DataSourceConfig, ExportConfig, ExportRequest, ExportResult, DEFAULT_LIMIT,
    DEFAULT_RETENTION_DAYS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct AppState {
    config: Arc<ExportConfig>,
    admin_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiExportRequest {
    user_id: String,
    from: String,
    to: String,
    include_brief_analysis: Option<bool>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ApiInstructionExportRequest {
    instruction: String,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

#[derive(Debug)]
struct ApiErrorResponse {
    status: StatusCode,
    message: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Default)]
struct CliOptions {
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let args = env::args().skip(1).collect::<Vec<_>>();
    let (command, option_args) = split_command(&args);
    match command {
        "serve" => run_serve(parse_options(option_args)?).await,
        "export" => {
            let options = parse_options(option_args)?;
            tokio::task::spawn_blocking(move || run_export(options))
                .await
                .context("export task failed")?
        }
        "cleanup" => {
            let options = parse_options(option_args)?;
            tokio::task::spawn_blocking(move || run_cleanup(options))
                .await
                .context("cleanup task failed")?
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => bail!("unknown command: {other}"),
    }
}

async fn run_serve(options: CliOptions) -> Result<()> {
    let config = Arc::new(config_from_options(&options)?);
    fs::create_dir_all(&config.export_dir)
        .with_context(|| format!("create {}", config.export_dir.display()))?;
    let removed = cleanup_exports(&config.export_dir, config.retention_days)?;
    if removed > 0 {
        info!(removed, "cleaned expired exports on startup");
    }

    let bind = option_or_env(&options, "bind", "NEWAPI_USAGE_BIND")
        .unwrap_or_else(|| "127.0.0.1:8098".to_owned())
        .parse::<SocketAddr>()
        .context("parse bind address")?;
    let admin_token = env::var("NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    if admin_token.is_none() {
        warn!("NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN is not set; bind to localhost only");
    }

    let state = AppState {
        config: Arc::clone(&config),
        admin_token,
    };
    spawn_cleanup_task(config);

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/usage-export", post(create_export_handler))
        .route(
            "/v1/usage-export/instruction",
            post(create_instruction_export_handler),
        )
        .route(
            "/v1/usage-export/{id}",
            get(get_export_handler).delete(delete_export_handler),
        )
        .route(
            "/v1/usage-export/{id}/download",
            get(download_export_handler),
        )
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(bind).await?;
    info!(%bind, "newapi usage exporter listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn run_export(options: CliOptions) -> Result<()> {
    let config = config_from_options(&options)?;
    let request = export_request_from_options(&options)?;
    let result = create_export(&config, &request)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_cleanup(options: CliOptions) -> Result<()> {
    let config = config_from_options(&options)?;
    let removed = cleanup_exports(&config.export_dir, config.retention_days)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({ "removed": removed }))?
    );
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_export_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ApiExportRequest>,
) -> Result<Json<ExportResult>, ApiErrorResponse> {
    require_auth(&headers, &state)?;
    let request = payload
        .into_export_request()
        .map_err(api_error_from_anyhow)?;
    let config = (*state.config).clone();
    let result = tokio::task::spawn_blocking(move || create_export(&config, &request))
        .await
        .map_err(|err| internal_error(format!("export task failed: {err}")))?
        .map_err(api_error_from_anyhow)?;
    Ok(Json(result))
}

async fn create_instruction_export_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ApiInstructionExportRequest>,
) -> Result<Json<ExportResult>, ApiErrorResponse> {
    require_auth(&headers, &state)?;
    let mut request =
        export_request_from_instruction(&payload.instruction).map_err(api_error_from_anyhow)?;
    if let Some(limit) = payload.limit {
        request.limit = limit;
    }
    let config = (*state.config).clone();
    let result = tokio::task::spawn_blocking(move || create_export(&config, &request))
        .await
        .map_err(|err| internal_error(format!("instruction export task failed: {err}")))?
        .map_err(api_error_from_anyhow)?;
    Ok(Json(result))
}

async fn get_export_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ExportResult>, ApiErrorResponse> {
    require_auth(&headers, &state)?;
    let metadata = load_export_metadata(&state.config.export_dir, &id)
        .await
        .map_err(api_error_from_anyhow)?;
    Ok(Json(metadata))
}

async fn download_export_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiErrorResponse> {
    require_auth(&headers, &state)?;
    let zip_path = export_zip_path(&state.config.export_dir, &id).map_err(api_error_from_anyhow)?;
    let bytes = tokio::fs::read(&zip_path)
        .await
        .map_err(|err| internal_error(format!("read export failed: {err}")))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{id}.zip\""))
            .map_err(|err| internal_error(format!("build content-disposition failed: {err}")))?,
    );
    Ok((headers, bytes).into_response())
}

async fn delete_export_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    require_auth(&headers, &state)?;
    let dir = export_entry_dir(&state.config.export_dir, &id).map_err(api_error_from_anyhow)?;
    if !dir.exists() {
        return Err(api_error(StatusCode::NOT_FOUND, "export not found"));
    }
    tokio::fs::remove_dir_all(&dir)
        .await
        .map_err(|err| internal_error(format!("delete export failed: {err}")))?;
    Ok(Json(json!({ "deleted": id })))
}

impl ApiExportRequest {
    fn into_export_request(self) -> Result<ExportRequest> {
        Ok(ExportRequest {
            user_id: self.user_id,
            from: parse_time(&self.from).context("parse from")?,
            to: parse_time(&self.to).context("parse to")?,
            include_brief_analysis: self.include_brief_analysis.unwrap_or(true),
            limit: self.limit.unwrap_or(DEFAULT_LIMIT),
        })
    }
}

async fn load_export_metadata(export_dir: &Path, export_id: &str) -> Result<ExportResult> {
    let metadata_path = export_entry_dir(export_dir, export_id)?.join("metadata.json");
    let bytes = tokio::fs::read(&metadata_path)
        .await
        .with_context(|| format!("read {}", metadata_path.display()))?;
    serde_json::from_slice(&bytes).context("parse export metadata")
}

fn export_entry_dir(export_dir: &Path, export_id: &str) -> Result<PathBuf> {
    if !export_id.starts_with("exp_")
        || export_id.contains('/')
        || export_id.contains('\\')
        || export_id.contains("..")
    {
        bail!("invalid export id");
    }
    Ok(export_dir.join(export_id))
}

fn require_auth(headers: &HeaderMap, state: &AppState) -> Result<(), ApiErrorResponse> {
    let Some(expected) = state.admin_token.as_deref() else {
        return Ok(());
    };
    let bearer_ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected);
    let api_key_ok = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if bearer_ok || api_key_ok {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::UNAUTHORIZED,
            "invalid exporter api key",
        ))
    }
}

fn spawn_cleanup_task(config: Arc<ExportConfig>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            let config = Arc::clone(&config);
            match tokio::task::spawn_blocking(move || {
                cleanup_exports(&config.export_dir, config.retention_days)
            })
            .await
            {
                Ok(Ok(removed)) if removed > 0 => {
                    info!(removed, "cleaned expired exports");
                }
                Ok(Ok(_)) => {}
                Ok(Err(err)) => warn!(error = %err, "cleanup failed"),
                Err(err) => warn!(error = %err, "cleanup task failed"),
            }
        }
    });
}

impl IntoResponse for ApiErrorResponse {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiError {
                error: self.message,
            }),
        )
            .into_response()
    }
}

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiErrorResponse {
    ApiErrorResponse {
        status,
        message: message.into(),
    }
}

fn api_error_from_anyhow(err: anyhow::Error) -> ApiErrorResponse {
    let message = err.to_string();
    let status = if message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.contains("required")
        || message.contains("range")
        || message.contains("limit")
        || message.contains("parse")
        || message.contains("invalid")
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    api_error(status, message)
}

fn internal_error(message: impl Into<String>) -> ApiErrorResponse {
    api_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn config_from_options(options: &CliOptions) -> Result<ExportConfig> {
    let data_source = if let Some(database_url) =
        option_or_env(options, "database_url", "NEWAPI_USAGE_DATABASE_URL")
            .or_else(|| option_or_env(options, "postgres_dsn", "NEWAPI_USAGE_POSTGRES_DSN"))
    {
        DataSourceConfig::Postgres(database_url)
    } else {
        let sqlite_path = option_or_env(options, "sqlite", "NEWAPI_USAGE_SQLITE_PATH")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    anyhow!(
                        "NEWAPI_USAGE_DATABASE_URL/--database-url or NEWAPI_USAGE_SQLITE_PATH/--sqlite is required"
                    )
                })?;
        DataSourceConfig::Sqlite(sqlite_path)
    };
    let export_dir = option_or_env(options, "export_dir", "NEWAPI_USAGE_EXPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("newapi-usage-exports"));
    let retention_days = option_or_env(options, "retention_days", "NEWAPI_USAGE_RETENTION_DAYS")
        .map(|value| value.parse::<i64>().context("parse retention days"))
        .transpose()?
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    let log_table = option_or_env(options, "log_table", "NEWAPI_USAGE_LOG_TABLE");
    Ok(ExportConfig {
        data_source,
        export_dir,
        retention_days,
        log_table,
    })
}

fn export_request_from_options(options: &CliOptions) -> Result<ExportRequest> {
    if let Some(instruction) = options.values.get("instruction") {
        let mut request = export_request_from_instruction(instruction)?;
        if let Some(limit) = options
            .values
            .get("limit")
            .map(|value| value.parse::<u32>().context("parse limit"))
            .transpose()?
        {
            request.limit = limit;
        }
        if options.flags.contains("no_brief_analysis") {
            request.include_brief_analysis = false;
        }
        return Ok(request);
    }

    let user_id = required_option(options, "user_id")?;
    let from = parse_time(&required_option(options, "from")?).context("parse from")?;
    let to = parse_time(&required_option(options, "to")?).context("parse to")?;
    let limit = options
        .values
        .get("limit")
        .map(|value| value.parse::<u32>().context("parse limit"))
        .transpose()?
        .unwrap_or(DEFAULT_LIMIT);
    Ok(ExportRequest {
        user_id,
        from,
        to,
        include_brief_analysis: !options.flags.contains("no_brief_analysis"),
        limit,
    })
}

fn required_option(options: &CliOptions, key: &str) -> Result<String> {
    options
        .values
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow!("--{} is required", key.replace('_', "-")))
}

fn option_or_env(options: &CliOptions, key: &str, env_key: &str) -> Option<String> {
    options
        .values
        .get(key)
        .cloned()
        .or_else(|| env::var(env_key).ok())
        .filter(|value| !value.trim().is_empty())
}

fn split_command(args: &[String]) -> (&str, &[String]) {
    if let Some(first) = args.first() {
        if matches!(
            first.as_str(),
            "serve" | "export" | "cleanup" | "help" | "-h" | "--help"
        ) {
            return (first, &args[1..]);
        }
    }
    ("serve", args)
}

fn parse_options(args: &[String]) -> Result<CliOptions> {
    let mut options = CliOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(raw_name) = arg.strip_prefix("--") else {
            bail!("unexpected argument: {arg}");
        };
        if raw_name.is_empty() {
            bail!("empty option name");
        }
        if let Some((name, value)) = raw_name.split_once('=') {
            options
                .values
                .insert(normalize_option(name), value.to_owned());
        } else {
            let name = normalize_option(raw_name);
            if index + 1 < args.len() && !args[index + 1].starts_with("--") {
                options.values.insert(name, args[index + 1].clone());
                index += 1;
            } else {
                options.flags.insert(name);
            }
        }
        index += 1;
    }
    Ok(options)
}

fn normalize_option(name: &str) -> String {
    name.replace('-', "_")
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn print_help() {
    println!(
        "\
newapi-usage-exporter

Commands:
  serve     Start HTTP API, default bind 127.0.0.1:8098
  export    Create one export from CLI
  cleanup   Delete expired export files

Config:
  --sqlite PATH                 or NEWAPI_USAGE_SQLITE_PATH
  --database-url URL_OR_DSN      or NEWAPI_USAGE_DATABASE_URL
  --export-dir PATH             or NEWAPI_USAGE_EXPORT_DIR
  --retention-days N            or NEWAPI_USAGE_RETENTION_DAYS, default 30
  --log-table NAME              or NEWAPI_USAGE_LOG_TABLE
  --bind HOST:PORT              or NEWAPI_USAGE_BIND, serve only

Export:
  --instruction 导出用户1从2026年6月5日~2026年6月5日的数据并做简要分析
  --user-id ID
  --from RFC3339_OR_YYYY-MM-DD
  --to RFC3339_OR_YYYY-MM-DD
  --limit N
  --no-brief-analysis

HTTP:
  GET    /health
  POST   /v1/usage-export
  POST   /v1/usage-export/instruction
  GET    /v1/usage-export/{{id}}
  GET    /v1/usage-export/{{id}}/download
  DELETE /v1/usage-export/{{id}}

Set NEWAPI_USAGE_EXPORTER_ADMIN_TOKEN when this API is reachable beyond localhost.
"
    );
}
