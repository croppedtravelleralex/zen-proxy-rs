use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use postgres::{Client as PgClient, NoTls, Row as PgRow};
use regex::Regex;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zip::write::SimpleFileOptions;

pub const DEFAULT_RETENTION_DAYS: i64 = 30;
pub const MAX_RANGE_DAYS: i64 = 31;
pub const DEFAULT_LIMIT: u32 = 200_000;

const LOG_TABLE_CANDIDATES: &[&str] = &["logs", "log", "usage_logs", "newapi_logs"];

const EXPORT_FIELDS: &[&str] = &[
    "log_id",
    "time",
    "user_id",
    "username",
    "token_id",
    "token_name",
    "model",
    "channel_id",
    "channel_name",
    "group",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "quota_cost",
    "status",
    "error_message_class",
    "duration_ms",
    "stream",
    "endpoint",
];

#[derive(Debug, Clone)]
pub struct ExportConfig {
    pub data_source: DataSourceConfig,
    pub export_dir: PathBuf,
    pub retention_days: i64,
    pub log_table: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DataSourceConfig {
    Sqlite(PathBuf),
    Postgres(String),
}

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub user_id: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub include_brief_analysis: bool,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub export_id: String,
    pub user_id: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub row_count: usize,
    pub download_path: String,
    pub zip_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedSchema {
    pub table: String,
    pub columns: HashMap<&'static str, String>,
    pub column_types: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub log_id: String,
    pub time: String,
    pub user_id: String,
    pub username: String,
    pub token_id: String,
    pub token_name: String,
    pub model: String,
    pub channel_id: String,
    pub channel_name: String,
    pub group: String,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub quota_cost: Option<f64>,
    pub status: String,
    pub error_message_class: String,
    pub duration_ms: Option<i64>,
    pub stream: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub request_count: usize,
    pub success_count: usize,
    pub error_count: usize,
    pub success_rate: f64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_tokens: i64,
    pub total_quota_cost: f64,
    pub long_output_count: usize,
    pub long_input_count: usize,
    pub high_input_low_output_count: usize,
    pub models: BTreeMap<String, usize>,
    pub channels: BTreeMap<String, usize>,
    pub errors: BTreeMap<String, usize>,
}

fn column_candidates() -> HashMap<&'static str, &'static [&'static str]> {
    HashMap::from([
        ("log_id", &["id", "log_id"][..]),
        (
            "created_at",
            &["created_at", "time", "created_time", "timestamp"][..],
        ),
        ("user_id", &["user_id", "userid", "user"][..]),
        ("username", &["username", "user_name", "name"][..]),
        ("token_id", &["token_id", "key_id"][..]),
        ("token_name", &["token_name", "key_name", "token"][..]),
        ("model", &["model_name", "model"][..]),
        ("channel_id", &["channel_id", "channel"][..]),
        ("channel_name", &["channel_name"][..]),
        ("group", &["group", "group_name"][..]),
        ("prompt_tokens", &["prompt_tokens", "input_tokens"][..]),
        (
            "completion_tokens",
            &["completion_tokens", "output_tokens"][..],
        ),
        ("total_tokens", &["total_tokens"][..]),
        ("quota_cost", &["quota", "quota_cost", "used_quota"][..]),
        ("status", &["status", "status_code", "type"][..]),
        ("error_message", &["error_message", "error_msg"][..]),
        (
            "duration_ms",
            &["duration_ms", "use_time", "elapsed_ms", "latency_ms"][..],
        ),
        ("stream", &["stream", "is_stream"][..]),
        ("endpoint", &["endpoint", "path", "request_path"][..]),
    ])
}

pub fn create_export(config: &ExportConfig, request: &ExportRequest) -> Result<ExportResult> {
    validate_request(request)?;
    cleanup_exports(&config.export_dir, config.retention_days)?;

    let (schema, records) = match &config.data_source {
        DataSourceConfig::Sqlite(path) => {
            let conn = open_read_only_sqlite(path)?;
            let schema = resolve_schema(&conn, config.log_table.as_deref())?;
            let records = fetch_usage_records(&conn, &schema, request)?;
            (schema, records)
        }
        DataSourceConfig::Postgres(database_url) => {
            let mut client = open_postgres(database_url)?;
            let schema = resolve_postgres_schema(&mut client, config.log_table.as_deref())?;
            let records = fetch_postgres_usage_records(&mut client, &schema, request)?;
            (schema, records)
        }
    };
    let summary = summarize(&records);

    let now = Utc::now();
    let export_id = format!(
        "exp_{}_{}",
        now.format("%Y%m%d%H%M%S"),
        Uuid::new_v4().simple()
    );
    let export_dir = config.export_dir.join(&export_id);
    fs::create_dir_all(&export_dir).with_context(|| format!("create {}", export_dir.display()))?;

    let expires_at = now + chrono::Duration::days(config.retention_days);
    let zip_path = export_dir.join("analysis_pack.zip");
    write_analysis_pack(
        &zip_path, request, &schema, &records, &summary, now, expires_at,
    )?;

    let result = ExportResult {
        export_id: export_id.clone(),
        user_id: request.user_id.clone(),
        from: request.from,
        to: request.to,
        created_at: now,
        expires_at,
        row_count: records.len(),
        download_path: format!("/v1/usage-export/{export_id}/download"),
        zip_path,
    };
    fs::write(
        export_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(result)
}

pub fn cleanup_exports(export_dir: &Path, retention_days: i64) -> Result<usize> {
    if !export_dir.exists() {
        return Ok(0);
    }
    let now = Utc::now();
    let cutoff = now - chrono::Duration::days(retention_days);
    let mut removed = 0;
    for entry in fs::read_dir(export_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let metadata_path = entry.path().join("metadata.json");
        let expired_by_metadata = fs::read(&metadata_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ExportResult>(&bytes).ok())
            .is_some_and(|metadata| metadata.expires_at <= now);
        let expired_by_mtime = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(DateTime::<Utc>::from)
            .is_some_and(|modified| modified <= cutoff);
        if expired_by_metadata || expired_by_mtime {
            fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn export_zip_path(export_dir: &Path, export_id: &str) -> Result<PathBuf> {
    if !export_id.starts_with("exp_")
        || export_id.contains('/')
        || export_id.contains('\\')
        || export_id.contains("..")
    {
        bail!("invalid export id");
    }
    let path = export_dir.join(export_id).join("analysis_pack.zip");
    if !path.exists() {
        bail!("export not found");
    }
    Ok(path)
}

fn validate_request(request: &ExportRequest) -> Result<()> {
    if request.user_id.trim().is_empty() {
        bail!("user_id is required");
    }
    if request.to < request.from {
        bail!("time range end must be after start");
    }
    if request.to - request.from > chrono::Duration::days(MAX_RANGE_DAYS) {
        bail!("time range too large: max {MAX_RANGE_DAYS} days");
    }
    if request.limit == 0 || request.limit > DEFAULT_LIMIT {
        bail!("limit must be between 1 and {DEFAULT_LIMIT}");
    }
    Ok(())
}

fn open_read_only_sqlite(path: &Path) -> Result<Connection> {
    let dsn = format!("file:{}?mode=ro", path.display());
    Connection::open_with_flags(
        dsn,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open read-only sqlite {}", path.display()))
}

pub fn resolve_schema(conn: &Connection, requested_table: Option<&str>) -> Result<ResolvedSchema> {
    let tables = table_names(conn)?;
    let table = requested_table
        .map(ToOwned::to_owned)
        .or_else(|| {
            LOG_TABLE_CANDIDATES
                .iter()
                .find(|candidate| tables.iter().any(|table| table == **candidate))
                .map(|value| (*value).to_owned())
        })
        .ok_or_else(|| anyhow!("cannot find NewAPI log table"))?;
    if !is_safe_identifier(&table) {
        bail!("unsafe table name");
    }

    let column_types = table_columns(conn, &table)?;
    let columns = resolve_columns(&column_types)?;
    Ok(ResolvedSchema {
        table,
        columns,
        column_types,
    })
}

fn resolve_columns(
    column_types: &HashMap<String, String>,
) -> Result<HashMap<&'static str, String>> {
    let lower_to_actual = column_types
        .keys()
        .map(|column| (column.to_ascii_lowercase(), column.clone()))
        .collect::<HashMap<_, _>>();
    let candidates = column_candidates();
    let mut columns = HashMap::new();
    for (field, names) in candidates {
        if let Some(actual) = names
            .iter()
            .find_map(|name| lower_to_actual.get(&name.to_ascii_lowercase()))
        {
            columns.insert(field, actual.clone());
        }
    }
    for required in ["created_at", "user_id"] {
        if !columns.contains_key(required) {
            bail!("cannot resolve required NewAPI log column: {required}");
        }
    }
    Ok(columns)
}

fn table_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("select name from sqlite_master where type='table'")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn table_columns(conn: &Connection, table: &str) -> Result<HashMap<String, String>> {
    let sql = format!("pragma table_info({})", quote_identifier(table)?);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        ))
    })?;
    rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        .map_err(Into::into)
}

fn open_postgres(database_url: &str) -> Result<PgClient> {
    PgClient::connect(database_url, NoTls).context("open read-only postgres")
}

pub fn resolve_postgres_schema(
    client: &mut PgClient,
    requested_table: Option<&str>,
) -> Result<ResolvedSchema> {
    let tables = postgres_table_names(client)?;
    let table = requested_table
        .map(ToOwned::to_owned)
        .or_else(|| {
            LOG_TABLE_CANDIDATES
                .iter()
                .find(|candidate| tables.iter().any(|table| table == **candidate))
                .map(|value| (*value).to_owned())
        })
        .ok_or_else(|| anyhow!("cannot find NewAPI log table"))?;
    if !is_safe_identifier(&table) {
        bail!("unsafe table name");
    }
    let column_types = postgres_table_columns(client, &table)?;
    let columns = resolve_columns(&column_types)?;
    Ok(ResolvedSchema {
        table,
        columns,
        column_types,
    })
}

fn postgres_table_names(client: &mut PgClient) -> Result<Vec<String>> {
    let rows = client.query(
        "select table_name from information_schema.tables where table_schema = 'public'",
        &[],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

fn postgres_table_columns(client: &mut PgClient, table: &str) -> Result<HashMap<String, String>> {
    let rows = client.query(
        "select column_name, data_type from information_schema.columns where table_schema = 'public' and table_name = $1",
        &[&table],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect())
}

fn fetch_usage_records(
    conn: &Connection,
    schema: &ResolvedSchema,
    request: &ExportRequest,
) -> Result<Vec<UsageRecord>> {
    let mut selected = schema
        .columns
        .iter()
        .map(|(field, column)| {
            Ok(format!(
                "{} as {}",
                quote_identifier(column)?,
                quote_identifier(field)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    selected.sort();
    let time_column = schema_column(schema, "created_at")?;
    let user_column = schema_column(schema, "user_id")?;
    let sql = format!(
        "select {} from {} where {} = ?1 and {} >= ?2 and {} <= ?3 order by {} asc limit ?4",
        selected.join(", "),
        quote_identifier(&schema.table)?,
        quote_identifier(user_column)?,
        quote_identifier(time_column)?,
        quote_identifier(time_column)?,
        quote_identifier(time_column)?,
    );
    let (from_value, to_value) = db_time_range(schema, request)?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![request.user_id, from_value, to_value, request.limit],
        |row| usage_record_from_row(row, schema),
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn fetch_postgres_usage_records(
    client: &mut PgClient,
    schema: &ResolvedSchema,
    request: &ExportRequest,
) -> Result<Vec<UsageRecord>> {
    let mut selected = schema
        .columns
        .iter()
        .map(|(field, column)| {
            Ok(format!(
                "{} as {}",
                quote_pg_identifier(column)?,
                quote_pg_identifier(field)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    selected.sort();
    let time_column = schema_column(schema, "created_at")?;
    let user_column = schema_column(schema, "user_id")?;
    let sql = format!(
        "select {} from {} where {}::text = $1 and {} >= $2 and {} <= $3 order by {} asc limit $4",
        selected.join(", "),
        quote_pg_identifier(&schema.table)?,
        quote_pg_identifier(user_column)?,
        quote_pg_identifier(time_column)?,
        quote_pg_identifier(time_column)?,
        quote_pg_identifier(time_column)?,
    );
    let limit = i64::from(request.limit);
    let rows = if postgres_time_is_integer(schema)? {
        let from_value = request.from.timestamp();
        let to_value = request.to.timestamp();
        client.query(&sql, &[&request.user_id, &from_value, &to_value, &limit])?
    } else {
        let from_value = request.from.to_rfc3339();
        let to_value = request.to.to_rfc3339();
        client.query(&sql, &[&request.user_id, &from_value, &to_value, &limit])?
    };
    rows.iter()
        .map(|row| usage_record_from_pg_row(row, schema))
        .collect()
}

fn usage_record_from_pg_row(row: &PgRow, schema: &ResolvedSchema) -> Result<UsageRecord> {
    let prompt_tokens = pg_optional_i64(row, "prompt_tokens");
    let completion_tokens = pg_optional_i64(row, "completion_tokens");
    let total_tokens = pg_optional_i64(row, "total_tokens").or_else(|| {
        if prompt_tokens.is_some() || completion_tokens.is_some() {
            Some(prompt_tokens.unwrap_or(0) + completion_tokens.unwrap_or(0))
        } else {
            None
        }
    });
    let status = pg_optional_string(row, "status").unwrap_or_default();
    let error = pg_optional_string(row, "error_message").unwrap_or_default();
    Ok(UsageRecord {
        log_id: pg_optional_string(row, "log_id").unwrap_or_default(),
        time: format_pg_record_time(row, schema)?,
        user_id: pg_optional_string(row, "user_id").unwrap_or_default(),
        username: pg_optional_string(row, "username").unwrap_or_default(),
        token_id: pg_optional_string(row, "token_id").unwrap_or_default(),
        token_name: pg_optional_string(row, "token_name").unwrap_or_default(),
        model: pg_optional_string(row, "model").unwrap_or_default(),
        channel_id: pg_optional_string(row, "channel_id").unwrap_or_default(),
        channel_name: pg_optional_string(row, "channel_name").unwrap_or_default(),
        group: pg_optional_string(row, "group").unwrap_or_default(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        quota_cost: pg_optional_f64(row, "quota_cost"),
        status: status.clone(),
        error_message_class: classify_error(&status, &error),
        duration_ms: pg_optional_i64(row, "duration_ms"),
        stream: pg_optional_string(row, "stream").unwrap_or_default(),
        endpoint: pg_optional_string(row, "endpoint").unwrap_or_default(),
    })
}

fn usage_record_from_row(row: &Row<'_>, schema: &ResolvedSchema) -> rusqlite::Result<UsageRecord> {
    let prompt_tokens = optional_i64(row, "prompt_tokens");
    let completion_tokens = optional_i64(row, "completion_tokens");
    let total_tokens = optional_i64(row, "total_tokens").or_else(|| {
        if prompt_tokens.is_some() || completion_tokens.is_some() {
            Some(prompt_tokens.unwrap_or(0) + completion_tokens.unwrap_or(0))
        } else {
            None
        }
    });
    let status = optional_string(row, "status").unwrap_or_default();
    let error = optional_string(row, "error_message").unwrap_or_default();
    Ok(UsageRecord {
        log_id: optional_string(row, "log_id").unwrap_or_default(),
        time: format_record_time(row, schema)?,
        user_id: optional_string(row, "user_id").unwrap_or_default(),
        username: optional_string(row, "username").unwrap_or_default(),
        token_id: optional_string(row, "token_id").unwrap_or_default(),
        token_name: optional_string(row, "token_name").unwrap_or_default(),
        model: optional_string(row, "model").unwrap_or_default(),
        channel_id: optional_string(row, "channel_id").unwrap_or_default(),
        channel_name: optional_string(row, "channel_name").unwrap_or_default(),
        group: optional_string(row, "group").unwrap_or_default(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        quota_cost: optional_f64(row, "quota_cost"),
        status: status.clone(),
        error_message_class: classify_error(&status, &error),
        duration_ms: optional_i64(row, "duration_ms"),
        stream: optional_string(row, "stream").unwrap_or_default(),
        endpoint: optional_string(row, "endpoint").unwrap_or_default(),
    })
}

fn optional_string(row: &Row<'_>, name: &str) -> Option<String> {
    match row.get_ref(name).ok()? {
        ValueRef::Null => None,
        ValueRef::Integer(value) => Some(value.to_string()),
        ValueRef::Real(value) => Some(value.to_string()),
        ValueRef::Text(value) => Some(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(_) => Some("[blob]".to_owned()),
    }
}

fn optional_i64(row: &Row<'_>, name: &str) -> Option<i64> {
    match row.get_ref(name).ok()? {
        ValueRef::Null => None,
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) => Some(value.round() as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.trim().parse().ok(),
        ValueRef::Blob(_) => None,
    }
}

fn optional_f64(row: &Row<'_>, name: &str) -> Option<f64> {
    match row.get_ref(name).ok()? {
        ValueRef::Null => None,
        ValueRef::Integer(value) => Some(value as f64),
        ValueRef::Real(value) => Some(value),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.trim().parse().ok(),
        ValueRef::Blob(_) => None,
    }
}

fn pg_optional_string(row: &PgRow, name: &str) -> Option<String> {
    if let Ok(value) = row.try_get::<_, Option<String>>(name) {
        return value;
    }
    if let Ok(value) = row.try_get::<_, Option<i64>>(name) {
        return value.map(|value| value.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(name) {
        return value.map(|value| value.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<f64>>(name) {
        return value.map(|value| value.to_string());
    }
    if let Ok(value) = row.try_get::<_, Option<bool>>(name) {
        return value.map(|value| value.to_string());
    }
    None
}

fn pg_optional_i64(row: &PgRow, name: &str) -> Option<i64> {
    if let Ok(value) = row.try_get::<_, Option<i64>>(name) {
        return value;
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(name) {
        return value.map(i64::from);
    }
    if let Ok(value) = row.try_get::<_, Option<i16>>(name) {
        return value.map(i64::from);
    }
    if let Ok(value) = row.try_get::<_, Option<f64>>(name) {
        return value.map(|value| value.round() as i64);
    }
    if let Ok(value) = row.try_get::<_, Option<String>>(name) {
        return value.and_then(|value| value.trim().parse().ok());
    }
    None
}

fn pg_optional_f64(row: &PgRow, name: &str) -> Option<f64> {
    if let Ok(value) = row.try_get::<_, Option<f64>>(name) {
        return value;
    }
    if let Ok(value) = row.try_get::<_, Option<i64>>(name) {
        return value.map(|value| value as f64);
    }
    if let Ok(value) = row.try_get::<_, Option<i32>>(name) {
        return value.map(f64::from);
    }
    if let Ok(value) = row.try_get::<_, Option<String>>(name) {
        return value.and_then(|value| value.trim().parse().ok());
    }
    None
}

fn format_record_time(row: &Row<'_>, schema: &ResolvedSchema) -> rusqlite::Result<String> {
    let time_column = schema_column(schema, "created_at")
        .map_err(|err| rusqlite::Error::InvalidParameterName(err.to_string()))?;
    let column_type = schema
        .column_types
        .get(time_column)
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if column_type.contains("int") {
        let timestamp = optional_i64(row, "created_at").unwrap_or_default();
        Ok(Utc
            .timestamp_opt(timestamp, 0)
            .single()
            .unwrap_or_else(Utc::now)
            .to_rfc3339())
    } else {
        Ok(optional_string(row, "created_at").unwrap_or_default())
    }
}

fn db_time_range(schema: &ResolvedSchema, request: &ExportRequest) -> Result<(String, String)> {
    let time_column = schema_column(schema, "created_at")?;
    let column_type = schema
        .column_types
        .get(time_column)
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if column_type.contains("int") {
        Ok((
            request.from.timestamp().to_string(),
            request.to.timestamp().to_string(),
        ))
    } else {
        Ok((request.from.to_rfc3339(), request.to.to_rfc3339()))
    }
}

fn postgres_time_is_integer(schema: &ResolvedSchema) -> Result<bool> {
    let time_column = schema_column(schema, "created_at")?;
    let column_type = schema
        .column_types
        .get(time_column)
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    Ok(column_type.contains("int"))
}

fn format_pg_record_time(row: &PgRow, schema: &ResolvedSchema) -> Result<String> {
    if postgres_time_is_integer(schema)? {
        let timestamp = pg_optional_i64(row, "created_at").unwrap_or_default();
        Ok(Utc
            .timestamp_opt(timestamp, 0)
            .single()
            .unwrap_or_else(Utc::now)
            .to_rfc3339())
    } else {
        Ok(pg_optional_string(row, "created_at").unwrap_or_default())
    }
}

fn schema_column<'a>(schema: &'a ResolvedSchema, field: &str) -> Result<&'a str> {
    schema
        .columns
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("schema missing field {field}"))
}

fn quote_identifier(value: &str) -> Result<String> {
    if !is_safe_identifier(value) {
        bail!("unsafe SQL identifier: {value}");
    }
    Ok(format!("`{value}`"))
}

fn quote_pg_identifier(value: &str) -> Result<String> {
    if !is_safe_identifier(value) {
        bail!("unsafe SQL identifier: {value}");
    }
    Ok(format!("\"{value}\""))
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub fn summarize(records: &[UsageRecord]) -> UsageSummary {
    let error_count = records
        .iter()
        .filter(|record| record.error_message_class != "ok")
        .count();
    let request_count = records.len();
    UsageSummary {
        request_count,
        success_count: request_count.saturating_sub(error_count),
        error_count,
        success_rate: if request_count == 0 {
            0.0
        } else {
            round4((request_count - error_count) as f64 / request_count as f64)
        },
        total_prompt_tokens: records
            .iter()
            .map(|record| record.prompt_tokens.unwrap_or(0))
            .sum(),
        total_completion_tokens: records
            .iter()
            .map(|record| record.completion_tokens.unwrap_or(0))
            .sum(),
        total_tokens: records
            .iter()
            .map(|record| record.total_tokens.unwrap_or(0))
            .sum(),
        total_quota_cost: round4(
            records
                .iter()
                .map(|record| record.quota_cost.unwrap_or(0.0))
                .sum(),
        ),
        long_output_count: records
            .iter()
            .filter(|record| record.completion_tokens.unwrap_or(0) >= 7_000)
            .count(),
        long_input_count: records
            .iter()
            .filter(|record| record.prompt_tokens.unwrap_or(0) >= 50_000)
            .count(),
        high_input_low_output_count: records
            .iter()
            .filter(|record| {
                record.prompt_tokens.unwrap_or(0) >= 20_000
                    && record.completion_tokens.unwrap_or(0) <= 500
            })
            .count(),
        models: grouped_counts(records.iter().map(|record| record.model.as_str())),
        channels: grouped_counts(records.iter().map(|record| record.channel_id.as_str())),
        errors: grouped_counts(
            records
                .iter()
                .map(|record| record.error_message_class.as_str()),
        ),
    }
}

fn grouped_counts<'a>(values: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        let key = if value.trim().is_empty() {
            "unknown"
        } else {
            value
        };
        *counts.entry(key.to_owned()).or_insert(0) += 1;
    }
    counts
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn classify_error(status: &str, error: &str) -> String {
    let text = format!("{status} {error}").to_ascii_lowercase();
    if text.trim().is_empty()
        || matches!(
            text.trim(),
            "ok" | "success" | "200" | "200 ok" | "2" | "type_2"
        )
    {
        return "ok".to_owned();
    }
    if text.contains("429") || text.contains("rate") {
        return "rate_limited".to_owned();
    }
    if text.contains("timeout") || text.contains("504") {
        return "timeout".to_owned();
    }
    if text.contains("channel") {
        return "channel_error".to_owned();
    }
    if text.contains("model") && (text.contains("not") || text.contains("unsupported")) {
        return "model_error".to_owned();
    }
    if text.contains("no assistant content") || text.contains("empty") {
        return "empty_output".to_owned();
    }
    if text.contains("json") || text.contains("deserialize") {
        return "protocol_error".to_owned();
    }
    if text.contains("400") {
        return "bad_request".to_owned();
    }
    if text.contains("500") || text.contains("502") || text.contains("503") {
        return "upstream_error".to_owned();
    }
    "other_error".to_owned()
}

fn write_analysis_pack(
    zip_path: &Path,
    request: &ExportRequest,
    schema: &ResolvedSchema,
    records: &[UsageRecord],
    summary: &UsageSummary,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    let mut zip_data = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut zip_data);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        add_zip_file(&mut zip, options, "usage_logs.csv", &usage_csv(records)?)?;
        add_zip_file(
            &mut zip,
            options,
            "usage_summary.json",
            &serde_json::to_vec_pretty(&json!({
                "metadata": {
                    "user_id": request.user_id,
                    "from": request.from,
                    "to": request.to,
                    "created_at": created_at,
                    "expires_at": expires_at,
                    "schema_table": schema.table,
                    "retention_note": "export files are short-lived and should be deleted after retention ttl"
                },
                "summary": summary
            }))?,
        )?;
        if request.include_brief_analysis {
            add_zip_file(
                &mut zip,
                options,
                "brief_analysis.md",
                brief_analysis(request, summary).as_bytes(),
            )?;
        }
        add_zip_file(
            &mut zip,
            options,
            "ai_analysis_guide.md",
            ai_analysis_guide().as_bytes(),
        )?;
        add_zip_file(
            &mut zip,
            options,
            "data_dictionary.md",
            data_dictionary().as_bytes(),
        )?;
        zip.finish()?;
    }
    fs::write(zip_path, zip_data.into_inner())?;
    Ok(())
}

fn add_zip_file<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    bytes: &[u8],
) -> Result<()> {
    zip.start_file(name, options)?;
    zip.write_all(bytes)?;
    Ok(())
}

fn usage_csv(records: &[UsageRecord]) -> Result<Vec<u8>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(EXPORT_FIELDS)?;
    for record in records {
        writer.write_record([
            record.log_id.as_str(),
            record.time.as_str(),
            record.user_id.as_str(),
            record.username.as_str(),
            record.token_id.as_str(),
            record.token_name.as_str(),
            record.model.as_str(),
            record.channel_id.as_str(),
            record.channel_name.as_str(),
            record.group.as_str(),
            &record
                .prompt_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            &record
                .completion_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            &record
                .total_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            &record
                .quota_cost
                .map(|value| value.to_string())
                .unwrap_or_default(),
            record.status.as_str(),
            record.error_message_class.as_str(),
            &record
                .duration_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            record.stream.as_str(),
            record.endpoint.as_str(),
        ])?;
    }
    Ok(writer.into_inner()?)
}

fn brief_analysis(request: &ExportRequest, summary: &UsageSummary) -> String {
    let mut lines = vec![
        "# NewAPI 使用简要分析".to_owned(),
        String::new(),
        "## 范围".to_owned(),
        String::new(),
        format!("- user_id: `{}`", request.user_id),
        format!("- from: `{}`", request.from.to_rfc3339()),
        format!("- to: `{}`", request.to.to_rfc3339()),
        String::new(),
        "## 客观事实".to_owned(),
        String::new(),
        format!("- 请求数：{}", summary.request_count),
        format!("- 成功率：{:.2}%", summary.success_rate * 100.0),
        format!("- prompt tokens：{}", summary.total_prompt_tokens),
        format!("- completion tokens：{}", summary.total_completion_tokens),
        format!("- total tokens：{}", summary.total_tokens),
        format!("- quota cost：{}", summary.total_quota_cost),
        format!(
            "- 长输出请求数（completion >= 7000）：{}",
            summary.long_output_count
        ),
        format!(
            "- 长输入请求数（prompt >= 50000）：{}",
            summary.long_input_count
        ),
        format!(
            "- 高输入低输出请求数：{}",
            summary.high_input_low_output_count
        ),
        String::new(),
        "## 初步建议".to_owned(),
        String::new(),
    ];
    if summary.error_count > 0 {
        lines.push("- 存在失败请求。先按错误类型排查模型、渠道、超时或协议问题。".to_owned());
    }
    if summary.long_output_count > 0 {
        lines.push("- 存在多次长输出。系统不能仅凭 tokens 判断用途，建议先询问用户主要是在写小说、写文档、写代码、通信协议还是其他任务。".to_owned());
    }
    if summary.long_input_count > 0 {
        lines.push(
            "- 存在长输入。建议确认是否重复携带历史上下文、日志、工具输出或大文件。".to_owned(),
        );
    }
    if summary.high_input_low_output_count > 0 {
        lines.push(
            "- 存在高输入低输出。可能是长上下文检索、调试或失败重试，建议结合实际用途拆分任务。"
                .to_owned(),
        );
    }
    if summary.error_count == 0
        && summary.long_output_count == 0
        && summary.long_input_count == 0
        && summary.high_input_low_output_count == 0
    {
        lines.push("- 暂未发现明显异常。可结合用户实际用途进一步优化提示词和模型选择。".to_owned());
    }
    lines.extend([
        String::new(),
        "## 需要用户补充的问题".to_owned(),
        String::new(),
        "1. 这些请求主要用于什么场景？例如编程、通信协议、写作、翻译、数据分析、客服。".to_owned(),
        "2. 长输出请求是否符合预期？如果不符合，是否希望更短、更结构化？".to_owned(),
        "3. 是否经常重复提交同一类上下文或日志？".to_owned(),
        "4. 更看重成本、速度、稳定性还是输出质量？".to_owned(),
        String::new(),
        "拿到用途后，再做针对性深度分析。不要仅凭 token 长度猜测用途。".to_owned(),
    ]);
    lines.join("\n") + "\n"
}

fn ai_analysis_guide() -> &'static str {
    "# AI 深度分析指南\n\n\
你是 AI 使用效率分析师。用户会提供 NewAPI 使用日志和简要汇总。\n\n\
分析规则：\n\n\
1. 先总结数据事实，不要猜用途。\n\
2. 如果看到长输出或长输入，只能说“长文本/长代码/长文档类任务”，不能直接判断是小说、通信、论文或代码。\n\
3. 先问用户 3-5 个问题确认主要用途。\n\
4. 用户说明用途后，再给领域建议。\n\
5. 每条建议必须引用数据证据，例如 tokens、失败率、模型、时间段或成本占比。\n\
6. 不要编造日志中没有的字段。\n\n\
用户用途示例：\n\n\
- 通信/协议/嵌入式：建议输出协议字段表、状态机、错误码、边界条件、测试向量，减少一次性超长自由文本。\n\
- 编程：建议按模块拆分、先接口后实现、要求测试用例和 diff。\n\
- 写作：建议章节拆分、人物/术语表、摘要和续写锚点。\n\
- 数据分析：建议先生成字段解释和统计结论，再做业务解释。\n\n\
输出格式：\n\n\
1. 数据事实\n\
2. 需要确认的问题\n\
3. 用户确认用途后的针对性建议\n\
4. 可节省成本的动作\n\
5. 可能影响质量的风险\n"
}

fn data_dictionary() -> &'static str {
    "# 字段说明\n\n\
- log_id: NewAPI 调用日志 ID。\n\
- time: 调用时间。\n\
- user_id: NewAPI 用户 ID。\n\
- username: 用户名，如 NewAPI 日志中可用。\n\
- token_id/token_name: API key 标识，不包含真实 key。\n\
- model: 请求模型。\n\
- channel_id/channel_name: NewAPI 渠道。\n\
- prompt_tokens: 输入 tokens。\n\
- completion_tokens: 输出 tokens。\n\
- total_tokens: 总 tokens。\n\
- quota_cost: NewAPI 记录的额度消耗。\n\
- status: NewAPI 原始状态字段。\n\
- error_message_class: 脱敏后的错误类别。\n\
- duration_ms: 请求耗时。\n\
- stream: 是否流式。\n\
- endpoint: 请求路径，如日志中可用。\n\n\
注意：导出包不包含 prompt 原文、完整响应、真实 API key 或 IP 明文。\n"
}

pub fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    let value = value.trim();
    if value.len() == 10 {
        return Ok(
            DateTime::parse_from_rfc3339(&format!("{value}T00:00:00+08:00"))?.with_timezone(&Utc),
        );
    }
    let normalized = value
        .strip_suffix('Z')
        .map_or_else(|| value.to_owned(), |prefix| format!("{prefix}+00:00"));
    Ok(DateTime::parse_from_rfc3339(&normalized)?.with_timezone(&Utc))
}

pub fn export_request_from_instruction(instruction: &str) -> Result<ExportRequest> {
    let user_id = parse_instruction_user_id(instruction)?;
    let dates = parse_instruction_dates(instruction)?;
    let include_brief_analysis = !instruction.contains("不要简要分析")
        && !instruction.contains("不做简要分析")
        && !instruction.contains("no brief");
    Ok(ExportRequest {
        user_id,
        from: date_start_utc(dates[0])?,
        to: date_start_utc(
            dates[1]
                .succ_opt()
                .ok_or_else(|| anyhow!("invalid instruction end date"))?,
        )?,
        include_brief_analysis,
        limit: DEFAULT_LIMIT,
    })
}

fn parse_instruction_user_id(instruction: &str) -> Result<String> {
    let re = Regex::new(r"(?i)(?:用户|user(?:_id)?|uid)\s*[:：#]?\s*([A-Za-z0-9_-]+)")?;
    let user_id = re
        .captures(instruction)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("cannot parse user id from instruction"))?;
    Ok(user_id)
}

fn parse_instruction_dates(instruction: &str) -> Result<Vec<NaiveDate>> {
    let re = Regex::new(
        r"(?x)
        (?:
            (?P<cy>\d{4})\s*年\s*(?P<cm>\d{1,2})\s*月\s*(?P<cd>\d{1,2})\s*(?:日|号)?
        )
        |
        (?:
            (?P<iy>\d{4})[-/](?P<im>\d{1,2})[-/](?P<id>\d{1,2})
        )",
    )?;
    let mut dates = Vec::new();
    for captures in re.captures_iter(instruction) {
        let parsed = if let (Some(year), Some(month), Some(day)) = (
            captures.name("cy"),
            captures.name("cm"),
            captures.name("cd"),
        ) {
            parse_naive_date(year.as_str(), month.as_str(), day.as_str())?
        } else if let (Some(year), Some(month), Some(day)) = (
            captures.name("iy"),
            captures.name("im"),
            captures.name("id"),
        ) {
            parse_naive_date(year.as_str(), month.as_str(), day.as_str())?
        } else {
            continue;
        };
        dates.push(parsed);
    }
    if dates.len() < 2 {
        bail!("cannot parse date range from instruction");
    }
    if dates[1] < dates[0] {
        bail!("instruction end date must be after start date");
    }
    Ok(dates.into_iter().take(2).collect())
}

fn parse_naive_date(year: &str, month: &str, day: &str) -> Result<NaiveDate> {
    NaiveDate::from_ymd_opt(year.parse()?, month.parse()?, day.parse()?)
        .ok_or_else(|| anyhow!("invalid instruction date"))
}

fn date_start_utc(date: NaiveDate) -> Result<DateTime<Utc>> {
    parse_time(&format!("{}T00:00:00+08:00", date.format("%Y-%m-%d")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    fn fixture_db() -> (TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("newapi.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "create table logs (
                id integer primary key,
                created_at integer not null,
                user_id integer not null,
                username text,
                token_name text,
                model_name text,
                channel_id integer,
                prompt_tokens integer,
                completion_tokens integer,
                quota integer,
                status text,
                error_message text,
                use_time integer,
                stream integer
            );",
        )
        .unwrap();
        let ts = parse_time("2026-06-01T12:00:00+08:00").unwrap().timestamp();
        conn.execute(
            "insert into logs values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                1,
                ts,
                123,
                "alice",
                "dev-key",
                "deepseek-v4-flash",
                69,
                1000,
                7200,
                8200,
                "ok",
                "",
                12000,
                1
            ],
        )
        .unwrap();
        (tmp, db_path)
    }

    #[test]
    fn rejects_range_over_31_days() {
        let request = ExportRequest {
            user_id: "1".to_owned(),
            from: parse_time("2026-06-01T00:00:00+08:00").unwrap(),
            to: parse_time("2026-07-10T00:00:00+08:00").unwrap(),
            include_brief_analysis: true,
            limit: DEFAULT_LIMIT,
        };
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn creates_analysis_pack_from_sqlite_logs() {
        let (_tmp, db_path) = fixture_db();
        let export_dir = tempfile::tempdir().unwrap();
        let config = ExportConfig {
            data_source: DataSourceConfig::Sqlite(db_path),
            export_dir: export_dir.path().to_path_buf(),
            retention_days: DEFAULT_RETENTION_DAYS,
            log_table: None,
        };
        let request = ExportRequest {
            user_id: "123".to_owned(),
            from: parse_time("2026-06-01T00:00:00+08:00").unwrap(),
            to: parse_time("2026-06-02T00:00:00+08:00").unwrap(),
            include_brief_analysis: true,
            limit: DEFAULT_LIMIT,
        };
        let result = create_export(&config, &request).unwrap();
        assert_eq!(result.row_count, 1);
        assert!(result.zip_path.exists());

        let file = fs::File::open(&result.zip_path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut summary = String::new();
        zip.by_name("usage_summary.json")
            .unwrap()
            .read_to_string(&mut summary)
            .unwrap();
        assert!(summary.contains("\"long_output_count\": 1"));
        let mut csv = String::new();
        zip.by_name("usage_logs.csv")
            .unwrap()
            .read_to_string(&mut csv)
            .unwrap();
        assert!(csv.contains("123"));
        assert!(csv.contains("69"));
    }

    #[test]
    fn can_skip_brief_analysis_file() {
        let (_tmp, db_path) = fixture_db();
        let export_dir = tempfile::tempdir().unwrap();
        let config = ExportConfig {
            data_source: DataSourceConfig::Sqlite(db_path),
            export_dir: export_dir.path().to_path_buf(),
            retention_days: DEFAULT_RETENTION_DAYS,
            log_table: None,
        };
        let request = ExportRequest {
            user_id: "123".to_owned(),
            from: parse_time("2026-06-01T00:00:00+08:00").unwrap(),
            to: parse_time("2026-06-02T00:00:00+08:00").unwrap(),
            include_brief_analysis: false,
            limit: DEFAULT_LIMIT,
        };
        let result = create_export(&config, &request).unwrap();
        let file = fs::File::open(&result.zip_path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        assert!(zip.by_name("brief_analysis.md").is_err());
        assert!(zip.by_name("ai_analysis_guide.md").is_ok());
    }

    #[test]
    fn cleanup_removes_expired_exports() {
        let export_dir = tempfile::tempdir().unwrap();
        let stale = export_dir.path().join("exp_stale");
        fs::create_dir_all(&stale).unwrap();
        let metadata = ExportResult {
            export_id: "exp_stale".to_owned(),
            user_id: "1".to_owned(),
            from: Utc::now(),
            to: Utc::now(),
            created_at: Utc::now() - chrono::Duration::days(40),
            expires_at: Utc::now() - chrono::Duration::days(1),
            row_count: 0,
            download_path: "/x".to_owned(),
            zip_path: stale.join("analysis_pack.zip"),
        };
        fs::write(
            stale.join("metadata.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let removed = cleanup_exports(export_dir.path(), DEFAULT_RETENTION_DAYS).unwrap();
        assert_eq!(removed, 1);
        assert!(!stale.exists());
    }

    #[test]
    fn parses_chinese_instruction_export_request() {
        let request = export_request_from_instruction(
            "导出用户1从2026年6月5日~2026年6月5日的数据并做简要分析",
        )
        .unwrap();
        assert_eq!(request.user_id, "1");
        assert_eq!(
            request.from,
            parse_time("2026-06-05T00:00:00+08:00").unwrap()
        );
        assert_eq!(request.to, parse_time("2026-06-06T00:00:00+08:00").unwrap());
        assert!(request.include_brief_analysis);
    }

    #[test]
    fn parses_iso_instruction_without_brief() {
        let request = export_request_from_instruction(
            "export user_id:abc from 2026-06-01 to 2026-06-03 不做简要分析",
        )
        .unwrap();
        assert_eq!(request.user_id, "abc");
        assert_eq!(
            request.from,
            parse_time("2026-06-01T00:00:00+08:00").unwrap()
        );
        assert_eq!(request.to, parse_time("2026-06-04T00:00:00+08:00").unwrap());
        assert!(!request.include_brief_analysis);
    }
}
