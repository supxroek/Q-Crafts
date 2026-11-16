use crate::core::query_ast::QueryAst;
use crate::core::schema_graph::SqlClient;
use crate::core::sql_compiler::{CompileMode, compile_to_tsql};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiberius::{ColumnData, Row};

/// โครงสร้างเก็บผลลัพธ์การพรีวิวข้อมูลจริง (แยกส่วนออกจาก UI เพื่อให้ใช้ได้ทั้ง CLI และ GUI)
#[derive(Serialize, Deserialize, Debug, Clone)]
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

    let stream = client.query(&compiled.sql, &[]).await.with_context(|| {
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
    let raw_cells: Vec<&ColumnData> = row.cells().map(|(_, data)| data).collect();
    let Some(cell) = raw_cells.get(index) else {
        return "NULL".to_string();
    };

    match cell {
        ColumnData::String(Some(value)) => truncate_text(value.trim(), 35),
        ColumnData::String(None)
        | ColumnData::U8(None)
        | ColumnData::I16(None)
        | ColumnData::I32(None)
        | ColumnData::I64(None)
        | ColumnData::F32(None)
        | ColumnData::F64(None)
        | ColumnData::Bit(None)
        | ColumnData::Guid(None)
        | ColumnData::Binary(None)
        | ColumnData::Numeric(None)
        | ColumnData::Xml(None)
        | ColumnData::DateTime(None)
        | ColumnData::SmallDateTime(None) => "NULL".to_string(),
        ColumnData::U8(Some(value)) => value.to_string(),
        ColumnData::I16(Some(value)) => value.to_string(),
        ColumnData::I32(Some(value)) => value.to_string(),
        ColumnData::I64(Some(value)) => value.to_string(),
        ColumnData::F32(Some(value)) => format!("{value:.2}"),
        ColumnData::F64(Some(value)) => format!("{value:.2}"),
        ColumnData::Bit(Some(value)) => {
            if *value {
                "true (1)".to_string()
            } else {
                "false (0)".to_string()
            }
        }
        ColumnData::Guid(Some(value)) => value.to_string(),
        ColumnData::Binary(Some(_)) => "<BINARY>".to_string(),
        ColumnData::Numeric(Some(value)) => value.to_string(),
        ColumnData::Xml(Some(value)) => truncate_text(&value.to_string(), 35),
        ColumnData::DateTime(Some(value)) => format_sql_datetime(
            value.days() + DAYS_FROM_YEAR_1_TO_1900,
            value.seconds_fragments() as u64 * 10_000_000 / 300,
        ),
        ColumnData::SmallDateTime(Some(value)) => format_sql_datetime(
            value.days() as i32 + DAYS_FROM_YEAR_1_TO_1900,
            value.seconds_fragments() as u64 * 60 * 10_000_000,
        ),
        ColumnData::Date(None)
        | ColumnData::DateTime2(None)
        | ColumnData::DateTimeOffset(None)
        | ColumnData::Time(None) => "NULL".to_string(),
        ColumnData::Date(Some(value)) => format_date(value.days()),
        ColumnData::DateTime2(Some(value)) => format_sql_datetime(
            value.date().days() as i32 - 1,
            value.time().increments() * 10u64.pow(7 - value.time().scale() as u32),
        ),
        ColumnData::DateTimeOffset(Some(value)) => {
            let datetime = value.datetime2();
            format_sql_datetime(
                datetime.date().days() as i32 - 1,
                datetime.time().increments() * 10u64.pow(7 - datetime.time().scale() as u32),
            )
        }
        ColumnData::Time(Some(value)) => {
            format_time(value.increments() * 10u64.pow(7 - value.scale() as u32))
        }
    }
}

const DAYS_FROM_YEAR_1_TO_1900: i32 = 693_595;

fn format_sql_datetime(days_from_year_1: i32, ticks_since_midnight: u64) -> String {
    let (year, month, day) = civil_date_from_days(days_from_year_1);
    let total_millis = ticks_since_midnight / 10_000;
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis % 3_600_000) / 60_000;
    let seconds = (total_millis % 60_000) / 1_000;
    let millis = total_millis % 1_000;
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn format_date(days_from_year_1: u32) -> String {
    let (year, month, day) = civil_date_from_days(days_from_year_1 as i32 - 1);
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_time(ticks_since_midnight: u64) -> String {
    let total_millis = ticks_since_midnight / 10_000;
    let hours = total_millis / 3_600_000;
    let minutes = (total_millis % 3_600_000) / 60_000;
    let seconds = (total_millis % 60_000) / 1_000;
    let millis = total_millis % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn civil_date_from_days(days_from_year_1: i32) -> (i32, u32, u32) {
    let z = days_from_year_1 - 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    (
        year + if month <= 2 { 1 } else { 0 },
        month as u32,
        day as u32,
    )
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.chars().count() > max_len {
        let short: String = text.chars().take(max_len - 3).collect();
        format!("{}...", short)
    } else {
        text.to_string()
    }
}
