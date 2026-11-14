use crate::config::app_config::{AppConfig, AuthMode};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Row};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

pub type SqlClient = Client<Compat<TcpStream>>;

/// ข้อมูลรายละเอียดของคอลัมน์พร้อมตัวอย่างข้อมูลจริง (Samples)
#[derive(Debug, Clone)]
pub struct ColumnDetail {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub sample_values: Vec<String>, // เก็บตัวอย่างข้อมูลจริง 1-3 ค่าที่ไม่ใช่ NULL
}

/// ข้อมูลความสัมพันธ์ Foreign Key สำหรับแนะนำการ JOIN อัตโนมัติ
#[derive(Debug, Clone)]
pub struct ForeignKeyRelation {
    pub constraint_name: String,
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

/// เชื่อมต่อไปยัง SQL Server (รองรับทั้ง Server รุ่นเก่า, Named Instance และ Windows Auth)
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

/// ดึงรายชื่อตารางทั้งหมดในฐานข้อมูล
pub async fn fetch_all_tables(client: &mut SqlClient) -> Result<Vec<String>> {
    let query = "
        SELECT TABLE_NAME 
        FROM INFORMATION_SCHEMA.TABLES 
        WHERE TABLE_TYPE = 'BASE TABLE' 
        ORDER BY TABLE_NAME ASC
    ";

    let stream = client.query(query, &[]).await?;
    let rows = stream.into_first_result().await?;

    let tables = rows
        .into_iter()
        .filter_map(|r| r.get::<&str, _>("TABLE_NAME").map(|s| s.to_string()))
        .collect();

    Ok(tables)
}

/// ดึงความสัมพันธ์ Foreign Keys ทั้งหมดที่เกี่ยวข้องกับตารางที่ระบุ (ทั้งตารางแม่และตารางลูก)
pub async fn fetch_related_foreign_keys(
    client: &mut SqlClient,
    table_name: &str,
) -> Result<Vec<ForeignKeyRelation>> {
    let query = "
        SELECT 
            fk.name AS ConstraintName,
            tp.name AS FromTable,
            cp.name AS FromColumn,
            tr.name AS ToTable,
            cr.name AS ToColumn
        FROM sys.foreign_keys fk
        INNER JOIN sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id
        INNER JOIN sys.tables tp ON fkc.parent_object_id = tp.object_id
        INNER JOIN sys.columns cp ON fkc.parent_object_id = cp.object_id AND fkc.parent_column_id = cp.column_id
        INNER JOIN sys.tables tr ON fkc.referenced_object_id = tr.object_id
        INNER JOIN sys.columns cr ON fkc.referenced_object_id = cr.object_id AND fkc.referenced_column_id = cr.column_id
        WHERE tp.name = @P1 OR tr.name = @P1
    ";

    let stream = client.query(query, &[&table_name]).await?;
    let rows = stream.into_first_result().await?;

    let mut relations = Vec::new();
    for row in rows {
        relations.push(ForeignKeyRelation {
            constraint_name: row
                .get::<&str, _>("ConstraintName")
                .unwrap_or("")
                .to_string(),
            from_table: row.get::<&str, _>("FromTable").unwrap_or("").to_string(),
            from_column: row.get::<&str, _>("FromColumn").unwrap_or("").to_string(),
            to_table: row.get::<&str, _>("ToTable").unwrap_or("").to_string(),
            to_column: row.get::<&str, _>("ToColumn").unwrap_or("").to_string(),
        });
    }

    Ok(relations)
}

/// ดึงโครงสร้างคอลัมน์พร้อมข้อมูลตัวอย่างด้วยกลไก 2-Tier Non-Null Sample Scan
pub async fn fetch_table_details_with_samples(
    client: &mut SqlClient,
    table_name: &str,
) -> Result<Vec<ColumnDetail>> {
    // 1. ดึงโครงสร้างคอลัมน์พื้นฐานและ Primary Key
    let schema_query = "
        SELECT 
            c.COLUMN_NAME,
            c.DATA_TYPE,
            c.IS_NULLABLE,
            CASE WHEN pk.COLUMN_NAME IS NOT NULL THEN 1 ELSE 0 END AS IS_PK
        FROM INFORMATION_SCHEMA.COLUMNS c
        LEFT JOIN (
            SELECT ku.TABLE_CATALOG, ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
            FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS AS tc
            INNER JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE AS ku
                ON tc.CONSTRAINT_TYPE = 'PRIMARY KEY' 
                AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
                AND tc.TABLE_NAME = ku.TABLE_NAME
        ) pk 
            ON c.TABLE_CATALOG = pk.TABLE_CATALOG
            AND c.TABLE_SCHEMA = pk.TABLE_SCHEMA
            AND c.TABLE_NAME = pk.TABLE_NAME
            AND c.COLUMN_NAME = pk.COLUMN_NAME
        WHERE c.TABLE_NAME = @P1
        ORDER BY c.ORDINAL_POSITION ASC
    ";

    let stream = client.query(schema_query, &[&table_name]).await?;
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

    // 2. ด่านที่ 1 (Fast Scan): ดึง TOP 50 แถวล่าสุดมาสกัดหาค่าที่ไม่ใช่ NULL
    // หาคอลัมน์สำหรับเรียงลำดับจากใหม่ไปเก่า (ใช้ PK ตัวแรก หรือคอลัมน์แรกของตาราง)
    let order_col = columns
        .iter()
        .find(|c| c.is_primary_key)
        .unwrap_or(&columns[0])
        .name
        .clone();

    // แปลงทุกคอลัมน์เป็น VARCHAR(100) ใน SQL เพื่อให้ Rust อ่านค่าออกมาเป็น String ได้ทุก Type (int, decimal, datetime)
    let cast_projections: Vec<String> = columns
        .iter()
        .filter(|c| !is_binary_type(&c.data_type))
        .map(|c| format!("TRY_CAST([{}] AS NVARCHAR(100)) AS [{}]", c.name, c.name))
        .collect();

    if !cast_projections.is_empty() {
        let fast_sample_sql = format!(
            "SELECT TOP 50 {} FROM [{}] ORDER BY [{}] DESC",
            cast_projections.join(", "),
            table_name,
            order_col
        );

        if let Ok(stream) = client.query(&fast_sample_sql, &[]).await {
            if let Ok(sample_rows) = stream.into_first_result().await {
                let mut collected: HashMap<String, HashSet<String>> = HashMap::new();

                for row in &sample_rows {
                    for col in &columns {
                        let entry = collected.entry(col.name.clone()).or_default();
                        if entry.len() >= 3 {
                            continue; // ได้ครบ 3 ตัวอย่างแล้ว ข้ามได้
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
            let fallback_sql = format!(
                "SELECT DISTINCT TOP 3 TRY_CAST([{}] AS NVARCHAR(100)) AS [val] \
                 FROM [{}] WHERE [{}] IS NOT NULL AND LTRIM(RTRIM(TRY_CAST([{}] AS NVARCHAR(100)))) <> ''",
                col.name, table_name, col.name, col.name
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

/// ฟังก์ชันสำหรับ On-Demand Column Inspector (เจาะลึกข้อมูลตัวอย่างและนับจำนวนความถี่ของคอลัมน์เดียว)
pub async fn inspect_column_distribution(
    client: &mut SqlClient,
    table_name: &str,
    column_name: &str,
) -> Result<Vec<(String, i32)>> {
    let sql = format!(
        "SELECT TOP 10 ISNULL(TRY_CAST([{}] AS NVARCHAR(100)), 'NULL') AS [Val], COUNT(*) AS [Cnt] \
         FROM [{}] GROUP BY [{}] ORDER BY [Cnt] DESC",
        column_name, table_name, column_name
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
/// ตรวจสอบว่าเป็นคอลัมน์ประเภทไฟล์/ไบนารีหรือไม่ (ไม่ควรดึงมาแสดงเป็นข้อความ)
fn is_binary_type(sql_type: &str) -> bool {
    matches!(
        sql_type.to_lowercase().as_str(),
        "varbinary" | "binary" | "image" | "timestamp" | "rowversion"
    )
}
/// ดึงข้อความจาก Row และตัดช่องว่างส่วนเกิน รวมถึงย่อข้อความที่ยาวเกิน 30 ตัวอักษร
fn extract_clean_string(row: &Row, col_name: &str) -> Option<String> {
    let raw = row.get::<&str, _>(col_name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // ย่อข้อความที่ยาวเกินไปเพื่อไม่ให้หน้าจอ CLI ล้น
    if trimmed.chars().count() > 28 {
        let short: String = trimmed.chars().take(25).collect();
        Some(format!("{}...", short))
    } else {
        Some(trimmed.to_string())
    }
}
