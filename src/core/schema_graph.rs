use crate::config::app_config::{AppConfig, AuthMode};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Row};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type SqlClient = Client<Compat<TcpStream>>;

/// ข้อมูลรายละเอียดของตาราง (Schema + Table Name)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
}

impl TableInfo {
    pub fn display_name(&self) -> String {
        if self.schema.eq_ignore_ascii_case("dbo") {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }
}

/// ข้อมูลรายละเอียดของคอลัมน์พร้อมตัวอย่างข้อมูลจริง (Samples)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ColumnDetail {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub sample_values: Vec<String>,
}

/// ข้อมูลความสัมพันธ์ Foreign Key สำหรับแนะนำการ JOIN อัตโนมัติ
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ForeignKeyRelation {
    pub constraint_name: String,
    pub from_schema: String,
    pub from_table: String,
    pub from_column: String,
    pub to_schema: String,
    pub to_table: String,
    pub to_column: String,
}

/// เชื่อมต่อไปยัง SQL Server
pub async fn connect(app_config: &AppConfig) -> Result<SqlClient> {
    let mut config = Config::new();

    if let Some((host_name, instance_name)) = app_config.host.split_once('\\') {
        config.host(host_name);
        config.instance_name(instance_name);
    } else {
        config.host(&app_config.host);
        config.port(app_config.port);
    }

    if !app_config.database.is_empty() {
        config.database(&app_config.database);
    }

    // ป้องกันปัญหา tls handshake eof สำหรับ SQL Server รุ่นเก่า
    config.encryption(EncryptionLevel::NotSupported);

    if app_config.trust_cert {
        config.trust_cert();
    }

    match app_config.auth_mode {
        AuthMode::SqlServer => {
            let user = app_config.username.as_deref().unwrap_or("");
            let pass = app_config.password.as_deref().unwrap_or("");
            config.authentication(AuthMethod::sql_server(user, pass));
        }
        AuthMode::Windows => {
            config.authentication(AuthMethod::Integrated);
        }
    }

    let tcp = TcpStream::connect(config.get_addr())
        .await
        .with_context(|| {
            format!(
                "ไม่สามารถเชื่อมต่อ TCP ไปที่ {}:{} ได้",
                app_config.host, app_config.port
            )
        })?;

    tcp.set_nodelay(true)?;

    let client = Client::connect(config, tcp.compat_write())
        .await
        .context("การยืนยันตัวตนหรือการเชื่อมต่อฐานข้อมูล SQL Server ล้มเหลว")?;

    Ok(client)
}

/// [ใหม่ v0.2] ดึงรายชื่อฐานข้อมูล (Databases) ทั้งหมดที่ออนไลน์อยู่ในเซิร์ฟเวอร์นี้ (ยกเว้น System DB)
pub async fn fetch_all_databases(client: &mut SqlClient) -> Result<Vec<String>> {
    let query = "
        SELECT name 
        FROM sys.databases 
        WHERE state_desc = 'ONLINE' 
          AND name NOT IN ('master', 'tempdb', 'model', 'msdb')
        ORDER BY name ASC
    ";

    let stream = client
        .query(query, &[])
        .await
        .context("ไม่สามารถดึงรายชื่อฐานข้อมูลจากเซิร์ฟเวอร์ได้")?;

    let rows = stream.into_first_result().await?;

    let dbs = rows
        .into_iter()
        .filter_map(|r| r.get::<&str, _>("name").map(|s| s.to_string()))
        .collect();

    Ok(dbs)
}

/// [อัปเกรด v0.2] ดึงรายชื่อตารางทั้งหมดจากฐานข้อมูลที่ระบุ (ข้าม Database ได้)
pub async fn fetch_tables_in_db(client: &mut SqlClient, database: &str) -> Result<Vec<TableInfo>> {
    let query = format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME 
         FROM [{}].INFORMATION_SCHEMA.TABLES 
         WHERE TABLE_TYPE = 'BASE TABLE' 
         ORDER BY TABLE_SCHEMA ASC, TABLE_NAME ASC",
        escape_identifier(database)
    );

    let stream = client
        .query(&query, &[])
        .await
        .with_context(|| format!("ไม่สามารถดึงรายชื่อตารางจากฐานข้อมูล '{}' ได้", database))?;

    let rows = stream.into_first_result().await?;

    let tables = rows
        .into_iter()
        .map(|r| TableInfo {
            schema: r
                .get::<&str, _>("TABLE_SCHEMA")
                .unwrap_or("dbo")
                .to_string(),
            name: r.get::<&str, _>("TABLE_NAME").unwrap_or("").to_string(),
        })
        .filter(|t| !t.name.is_empty())
        .collect();

    Ok(tables)
}

/// [อัปเกรด v0.2] ดึงความสัมพันธ์ Foreign Keys ภายในฐานข้อมูลที่ระบุ
pub async fn fetch_related_foreign_keys(
    client: &mut SqlClient,
    database: &str,
    schema: &str,
    table_name: &str,
) -> Result<Vec<ForeignKeyRelation>> {
    let safe_db = escape_identifier(database);
    let query = format!(
        "SELECT 
            fk.name AS ConstraintName,
            sp.name AS FromSchema,
            tp.name AS FromTable,
            cp.name AS FromColumn,
            sr.name AS ToSchema,
            tr.name AS ToTable,
            cr.name AS ToColumn
        FROM [{db}].sys.foreign_keys fk
        INNER JOIN [{db}].sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id
        INNER JOIN [{db}].sys.tables tp ON fkc.parent_object_id = tp.object_id
        INNER JOIN [{db}].sys.schemas sp ON tp.schema_id = sp.schema_id
        INNER JOIN [{db}].sys.columns cp ON fkc.parent_object_id = cp.object_id AND fkc.parent_column_id = cp.column_id
        INNER JOIN [{db}].sys.tables tr ON fkc.referenced_object_id = tr.object_id
        INNER JOIN [{db}].sys.schemas sr ON tr.schema_id = sr.schema_id
        INNER JOIN [{db}].sys.columns cr ON fkc.referenced_object_id = cr.object_id AND fkc.referenced_column_id = cr.column_id
        WHERE (tp.name = @P1 AND sp.name = @P2) OR (tr.name = @P1 AND sr.name = @P2)",
        db = safe_db
    );

    let stream = client.query(&query, &[&table_name, &schema]).await?;
    let rows = stream.into_first_result().await?;

    let mut relations = Vec::new();
    for row in rows {
        relations.push(ForeignKeyRelation {
            constraint_name: row
                .get::<&str, _>("ConstraintName")
                .unwrap_or("")
                .to_string(),
            from_schema: row
                .get::<&str, _>("FromSchema")
                .unwrap_or("dbo")
                .to_string(),
            from_table: row.get::<&str, _>("FromTable").unwrap_or("").to_string(),
            from_column: row.get::<&str, _>("FromColumn").unwrap_or("").to_string(),
            to_schema: row.get::<&str, _>("ToSchema").unwrap_or("dbo").to_string(),
            to_table: row.get::<&str, _>("ToTable").unwrap_or("").to_string(),
            to_column: row.get::<&str, _>("ToColumn").unwrap_or("").to_string(),
        });
    }

    Ok(relations)
}

/// [อัปเกรด v0.2] ดึงโครงสร้างคอลัมน์พร้อมตัวอย่างข้อมูลจริงข้าม Database ได้ (2-Tier Non-Null Sample Scan)
pub async fn fetch_table_details_with_samples(
    client: &mut SqlClient,
    database: &str,
    schema: &str,
    table_name: &str,
) -> Result<Vec<ColumnDetail>> {
    let safe_db = escape_identifier(database);
    let safe_schema = escape_identifier(schema);
    let safe_table = escape_identifier(table_name);

    // 1. ดึงโครงสร้างคอลัมน์และ Primary Key จากฐานข้อมูลเป้าหมาย
    let schema_query = format!(
        "SELECT 
            c.COLUMN_NAME,
            c.DATA_TYPE,
            c.IS_NULLABLE,
            CASE WHEN pk.COLUMN_NAME IS NOT NULL THEN 1 ELSE 0 END AS IS_PK
        FROM [{db}].INFORMATION_SCHEMA.COLUMNS c
        LEFT JOIN (
            SELECT ku.TABLE_CATALOG, ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
            FROM [{db}].INFORMATION_SCHEMA.TABLE_CONSTRAINTS AS tc
            INNER JOIN [{db}].INFORMATION_SCHEMA.KEY_COLUMN_USAGE AS ku
                ON tc.CONSTRAINT_TYPE = 'PRIMARY KEY' 
                AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
                AND tc.TABLE_SCHEMA = ku.TABLE_SCHEMA
                AND tc.TABLE_NAME = ku.TABLE_NAME
        ) pk 
            ON c.TABLE_CATALOG = pk.TABLE_CATALOG
            AND c.TABLE_SCHEMA = pk.TABLE_SCHEMA
            AND c.TABLE_NAME = pk.TABLE_NAME
            AND c.COLUMN_NAME = pk.COLUMN_NAME
        WHERE c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2
        ORDER BY c.ORDINAL_POSITION ASC",
        db = safe_db
    );

    let stream = client.query(&schema_query, &[&schema, &table_name]).await?;
    let schema_rows = stream.into_first_result().await?;

    let mut columns = Vec::new();
    for row in schema_rows {
        columns.push(ColumnDetail {
            name: row.get::<&str, _>("COLUMN_NAME").unwrap_or("").to_string(),
            data_type: row.get::<&str, _>("DATA_TYPE").unwrap_or("").to_string(),
            is_nullable: row.get::<&str, _>("IS_NULLABLE") == Some("YES"),
            is_primary_key: row.get::<i32, _>("IS_PK").unwrap_or(0) == 1,
            sample_values: Vec::new(),
        });
    }

    if columns.is_empty() {
        return Ok(columns);
    }

    // 2. ด่านที่ 1 (Fast Scan): ดึง TOP 50 แถวล่าสุดจาก [Database].[Schema].[Table]
    let order_col = columns
        .iter()
        .find(|c| c.is_primary_key)
        .unwrap_or(&columns[0])
        .name
        .clone();

    let cast_projections: Vec<String> = columns
        .iter()
        .filter(|c| !is_binary_type(&c.data_type))
        .map(|c| {
            let safe_col = escape_identifier(&c.name);
            format!(
                "TRY_CAST([{}] AS NVARCHAR(100)) AS [{}]",
                safe_col, safe_col
            )
        })
        .collect();

    if !cast_projections.is_empty() {
        let fast_sample_sql = format!(
            "SELECT TOP 50 {} FROM [{}].[{}].[{}] ORDER BY [{}] DESC",
            cast_projections.join(", "),
            safe_db,
            safe_schema,
            safe_table,
            escape_identifier(&order_col)
        );

        if let Ok(stream) = client.query(&fast_sample_sql, &[]).await {
            if let Ok(sample_rows) = stream.into_first_result().await {
                let mut collected: HashMap<String, HashSet<String>> = HashMap::new();

                for row in &sample_rows {
                    for col in &columns {
                        let entry = collected.entry(col.name.clone()).or_default();
                        if entry.len() >= 3 {
                            continue;
                        }
                        if let Some(val) = extract_clean_string(row, &col.name) {
                            entry.insert(val);
                        }
                    }
                }
                for col in &mut columns {
                    if let Some(vals) = collected.remove(&col.name) {
                        col.sample_values = vals.into_iter().collect();
                    }
                }
            }
        }
    }
    // 3. ด่านที่ 2 (Fallback Scan): คอลัมน์ไหนใน 50 แถวแรกยังว่าง (NULL) ให้ยิงคิวรีหาค่าจริงเฉพาะคอลัมน์นั้น
    for col in &mut columns {
        if col.sample_values.is_empty() && !is_binary_type(&col.data_type) {
            let safe_col = escape_identifier(&col.name);
            let fallback_sql = format!(
                "SELECT DISTINCT TOP 3 TRY_CAST([{}] AS NVARCHAR(100)) AS [val] 
FROM [{}].[{}].[{}] WHERE [{}] IS NOT NULL AND LTRIM(RTRIM(TRY_CAST([{}] AS NVARCHAR(100)))) <> ''",
                safe_col, safe_db, safe_schema, safe_table, safe_col, safe_col
            );
            if let Ok(stream) = client.query(&fallback_sql, &[]).await {
                if let Ok(rows) = stream.into_first_result().await {
                    for r in rows {
                        if let Some(val) = extract_clean_string(&r, "val") {
                            col.sample_values.push(val);
                        }
                    }
                }
            }
        }
    }
    Ok(columns)
}
/// [อัปเกรด v0.2] ฟังก์ชันสำหรับ On-Demand Column Inspector รองรับระบุ Database
pub async fn inspect_column_distribution(
    client: &mut SqlClient,
    database: &str,
    schema: &str,
    table_name: &str,
    column_name: &str,
) -> Result<Vec<(String, i32)>> {
    let safe_db = escape_identifier(database);
    let safe_schema = escape_identifier(schema);
    let safe_table = escape_identifier(table_name);
    let safe_col = escape_identifier(column_name);
    let sql = format!(
        "SELECT TOP 10 ISNULL(TRY_CAST([{}] AS NVARCHAR(100)), 'NULL') AS [Val], COUNT(*) AS [Cnt] 
FROM [{}].[{}].[{}] GROUP BY [{}] ORDER BY [Cnt] DESC",
        safe_col, safe_db, safe_schema, safe_table, safe_col
    );
    let stream = client.query(&sql, &[]).await?;
    let rows = stream.into_first_result().await?;
    let mut stats = Vec::new();
    for r in rows {
        let val = r.get::<&str, _>("Val").unwrap_or("NULL").to_string();
        let cnt = r.get::<i32, _>("Cnt").unwrap_or(0);
        stats.push((val, cnt));
    }
    Ok(stats)
}
/// ป้องกัน SQL Injection สำหรับชื่อ Identifier (Database, Schema, Table, Column)
fn escape_identifier(ident: &str) -> String {
    ident.replace(']', "]]")
}
fn is_binary_type(sql_type: &str) -> bool {
    matches!(
        sql_type.to_lowercase().as_str(),
        "varbinary" | "binary" | "image" | "timestamp" | "rowversion"
    )
}
fn extract_clean_string(row: &Row, col_name: &str) -> Option<String> {
    let raw = row.get::<&str, _>(col_name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > 28 {
        let short: String = trimmed.chars().take(25).collect();
        Some(format!("{}...", short))
    } else {
        Some(trimmed.to_string())
    }
}
