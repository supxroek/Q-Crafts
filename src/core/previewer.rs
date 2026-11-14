use crate::core::query_ast::QueryAst;
use crate::core::schema_graph::SqlClient;
use crate::core::sql_compiler::{compile_to_tsql, CompileMode};
use anyhow::{Context, Result};
use tiberius::{ColumnData, Row};

/// โครงสร้างเก็บผลลัพธ์การพรีวิวข้อมูลจริง (แยกส่วนออกจาก UI เพื่อให้ใช้ได้ทั้ง CLI และ GUI)
#[derive(Debug, Clone)]
pub struct PreviewResult {
    pub executed_sql: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// ยิงคำสั่ง SELECT TOP N ไปยัง SQL Server เพื่อดึงข้อมูลตัวอย่างมาแสดงผล
pub async fn run_live_preview(
    client: &mut SqlClient,
    ast: &QueryAst,
    limit: u32,
) -> Result<PreviewResult> {
    let compiled = compile_to_tsql(ast, CompileMode::Preview(limit));

    let stream = client
        .query(&compiled.sql, &[])
        .await
        .with_context(|| {
            format!(
                "เกิดข้อผิดพลาดขณะทดสอบรัน SQL Query:\n{}\nกรุณาตรวจสอบเงื่อนไข WHERE หรือการ JOIN",
                compiled.sql
            )
        })?;

    let db_rows = stream.into_first_result().await?;

    // ดึงชื่อหัวตารางจากผลลัพธ์จริงที่ SQL Server ตอบกลับมา
    let headers: Vec<String> = if let Some(first_row) = db_rows.first() {
        first_row
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect()
    } else {
        compiled
            .resolved_columns
            .iter()
            .map(|c| c.final_name.clone())
            .collect()
    };

    // แปลงข้อมูลทุกแถวและทุกคอลัมน์ให้กลายเป็น String ที่อ่านง่าย
    let mut formatted_rows = Vec::with_capacity(db_rows.len());
    for row in &db_rows {
        let mut row_vals = Vec::with_capacity(headers.len());
        for idx in 0..headers.len() {
            row_vals.push(format_column_value(row, idx));
        }
        formatted_rows.push(row_vals);
    }

    Ok(PreviewResult {
        executed_sql: compiled.sql,
        headers,
        rows: formatted_rows,
    })
}

/// แปลงข้อมูลดิบแต่ละชนิดของ SQL Server (ColumnData) ให้เป็นข้อความที่อ่านง่าย
fn format_column_value(row: &Row, index: usize) -> String {
    // ตรวจสอบและดึงค่าตามชนิดข้อมูลของ Tiberius
    if let Ok(Some(val)) = row.try_get::<&str, _>(index) {
        return truncate_text(val.trim(), 35);
    }
    if let Ok(Some(val)) = row.try_get::<i32, _>(index) {
        return val.to_string();
    }
    if let Ok(Some(val)) = row.try_get::<i64, _>(index) {
        return val.to_string();
    }
    if let Ok(Some(val)) = row.try_get::<i16, _>(index) {
        return val.to_string();
    }
    if let Ok(Some(val)) = row.try_get::<u8, _>(index) {
        return val.to_string();
    }
    if let Ok(Some(val)) = row.try_get::<bool, _>(index) {
        return if val { "true (1)".to_string() } else { "false (0)".to_string() };
    }
    if let Ok(Some(val)) = row.try_get::<f64, _>(index) {
        return format!("{:.2}", val);
    }
    if let Ok(Some(val)) = row.try_get::<f32, _>(index) {
        return format!("{:.2}", val);
    }
    if let Ok(Some(val)) = row.try_get::<tiberius::numeric::Numeric, _>(index) {
        return val.to_string();
    }
    if let Ok(Some(val)) = row.try_get::<tiberius::Uuid, _>(index) {
        return val.to_string();
    }

    // กรณีเป็นค่าว่าง (NULL) หรือชนิดข้อมูลวันที่/ไบนารี
    match row.try_get::<&[u8], _>(index) {
        Ok(Some(_)) => "<BINARY>".to_string(),
        _ => {
            // ใช้ Debug format เป็นตัวช่วยสุดท้ายสำหรับ DateTime / Date / Time
            let raw_cells: Vec<&ColumnData> = row.cells().map(|(_, data)| data).collect();
            if let Some(cell) = raw_cells.get(index) {
                let debug_str = format!("{:?}", cell);
                if debug_str == "Null" {
                    "NULL".to_string()
                } else {
                    truncate_text(&debug_str, 35)
                }
            } else {
                "NULL".to_string()
            }
        }
    }
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.chars().count() > max_len {
        let short: String = text.chars().take(max_len - 3).collect();
        format!("{}...", short)
    } else {
        text.to_string()
    }
}
