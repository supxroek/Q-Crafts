use crate::core::query_ast::{
    AggregateFunc, CompareOperator, FilterValue, QueryAst, SelectField,
};
use std::collections::HashMap;

/// โหมดการคอมไพล์ SQL
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompileMode {
    /// โหมดส่งออกโค้ดจริง (รักษาชื่อ @Parameter ไว้)
    Export,
    /// โหมดพรีวิวข้อมูลจริง (แทนที่ @Parameter ด้วยค่าทดสอบชั่วคราว และใส่ TOP limit)
    Preview(u32),
}

/// ผลลัพธ์ที่ได้จากการคอมไพล์ QueryAst
#[derive(Debug, Clone)]
pub struct CompiledQuery {
    pub sql: String,
    /// รายชื่อคอลัมน์สุดท้ายที่จะปรากฏในผลลัพธ์ (พร้อมชื่อ Alias ที่ถูกแก้การชนกันแล้ว)
    pub resolved_columns: Vec<ResolvedColumn>,
}

#[derive(Debug, Clone)]
pub struct ResolvedColumn {
    pub final_name: String,    // ชื่อคอลัมน์ในผลลัพธ์ เช่น "CustomerName" หรือ "OrderNo"
    pub sql_data_type: String, // Data Type เดิมใน SQL Server
}

/// แปลง QueryAst ให้กลายเป็นคำสั่ง T-SQL ที่สมบูรณ์ (รองรับ Cross-Database 3-Part Naming)
pub fn compile_to_tsql(ast: &QueryAst, mode: CompileMode) -> CompiledQuery {
    let mut sql_lines = Vec::new();
    let mut resolved_columns = Vec::new();

    // -------------------------------------------------------------------------
    // 1. ส่วนของ SELECT และจัดการชื่อคอลัมน์ซ้ำอัตโนมัติ (Auto-Aliasing)
    // -------------------------------------------------------------------------
    let top_clause = match mode {
        CompileMode::Preview(limit) => format!("TOP ({}) ", limit),
        CompileMode::Export => match ast.top_limit {
            Some(n) => format!("TOP ({}) ", n),
            None => String::new(),
        },
    };

    if ast.select_columns.is_empty() {
        sql_lines.push(format!("SELECT {}{}.*", top_clause, ast.main_table.alias));
    } else {
        // นับความถี่ของชื่อคอลัมน์เพื่อตรวจสอบการซ้ำกัน (เช่น vhd.RecId และ vdt.RecId)
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for field in &ast.select_columns {
            let effective_name = field
                .output_alias
                .clone()
                .unwrap_or_else(|| field.column.column_name.clone());
            *name_counts.entry(effective_name.to_lowercase()).or_insert(0) += 1;
        }

        let mut projections = Vec::new();
        for field in &ast.select_columns {
            let (expr, final_col_name) = build_select_expression(field, &name_counts);
            projections.push(format!("    {}", expr));
            resolved_columns.push(ResolvedColumn {
                final_name: final_col_name,
                sql_data_type: field.sql_data_type.clone(),
            });
        }

        sql_lines.push(format!("SELECT {}", top_clause.trim_end()));
        sql_lines.push(projections.join(",\n"));
    }

    // -------------------------------------------------------------------------
    // 2. ส่วนของ FROM และ JOIN หลายตาราง (อ้างอิงเต็มรูปแบบ [Database].[Schema].[Table])
    // -------------------------------------------------------------------------
    sql_lines.push(format!(
        "FROM {} AS {}",
        ast.main_table.full_sql_path(),
        ast.main_table.alias
    ));

    for join in &ast.joins {
        sql_lines.push(format!(
            "{} {} AS {} ON {}.[{}] = {}.[{}]",
            join.join_type.as_sql(),
            join.target_table.full_sql_path(),
            join.target_table.alias,
            join.left_column.table_alias,
            join.left_column.column_name,
            join.right_column.table_alias,
            join.right_column.column_name,
        ));
    }

    // -------------------------------------------------------------------------
    // 3. ส่วนของ WHERE (รองรับทั้ง Static, Auto-Rules และ @Parameters)
    // -------------------------------------------------------------------------
    if !ast.where_conditions.is_empty() {
        let mut conditions = Vec::new();

        for cond in &ast.where_conditions {
            let col_expr = format!("{}.[{}]", cond.column.table_alias, cond.column.column_name);
            let op_sql = cond.operator.as_sql();

            let clause = if !cond.operator.requires_value() {
                format!("{} {}", col_expr, op_sql)
            } else {
                match &cond.value {
                    FilterValue::Static {
                        raw_value,
                        is_numeric,
                    } => {
                        let formatted_val =
                            format_sql_literal(raw_value, *is_numeric, &cond.operator);
                        format!("{} {} {}", col_expr, op_sql, formatted_val)
                    }
                    FilterValue::Parameter {
                        param_name,
                        sample_value,
                        is_numeric,
                    } => match mode {
                        CompileMode::Export => {
                            let clean_param = param_name.trim_start_matches('@');
                            format!("{} {} @{}", col_expr, op_sql, clean_param)
                        }
                        CompileMode::Preview(_) => {
                            let test_val = sample_value.as_deref().unwrap_or("");
                            let formatted_val =
                                format_sql_literal(test_val, *is_numeric, &cond.operator);
                            format!("{} {} {}", col_expr, op_sql, formatted_val)
                        }
                    },
                    FilterValue::None => format!("{} {}", col_expr, op_sql),
                }
            };

            conditions.push(clause);
        }

        sql_lines.push(format!("WHERE {}", conditions.join("\n  AND ")));
    }

    // -------------------------------------------------------------------------
    // 4. ส่วนของ GROUP BY
    // -------------------------------------------------------------------------
    if !ast.group_by.is_empty() {
        let groups: Vec<String> = ast
            .group_by
            .iter()
            .map(|c| format!("{}.[{}]", c.table_alias, c.column_name))
            .collect();
        sql_lines.push(format!("GROUP BY {}", groups.join(", ")));
    }

    // -------------------------------------------------------------------------
    // 5. ส่วนของ ORDER BY
    // -------------------------------------------------------------------------
    if !ast.order_by.is_empty() {
        let orders: Vec<String> = ast
            .order_by
            .iter()
            .map(|o| {
                format!(
                    "{}.[{}] {}",
                    o.column.table_alias,
                    o.column.column_name,
                    o.direction.as_sql()
                )
            })
            .collect();
        sql_lines.push(format!("ORDER BY {}", orders.join(", ")));
    }

    CompiledQuery {
        sql: sql_lines.join("\n"),
        resolved_columns,
    }
}

/// สร้าง Expression ของคอลัมน์ใน SELECT พร้อมเติม AS อัตโนมัติหากชื่อซ้ำหรือใช้ Aggregate
fn build_select_expression(
    field: &SelectField,
    name_counts: &HashMap<String, usize>,
) -> (String, String) {
    let base_col = format!("{}.[{}]", field.column.table_alias, field.column.column_name);

    let expr_with_agg = match field.aggregate {
        AggregateFunc::None => base_col,
        AggregateFunc::Count => format!("COUNT({})", base_col),
        AggregateFunc::Sum => format!("SUM({})", base_col),
        AggregateFunc::Avg => format!("AVG({})", base_col),
        AggregateFunc::Min => format!("MIN({})", base_col),
        AggregateFunc::Max => format!("MAX({})", base_col),
    };

    if let Some(custom_alias) = &field.output_alias {
        return (
            format!("{} AS [{}]", expr_with_agg, custom_alias),
            custom_alias.clone(),
        );
    }

    if field.aggregate != AggregateFunc::None {
        let auto_agg_name = format!("{:?}_{}", field.aggregate, field.column.column_name);
        return (
            format!("{} AS [{}]", expr_with_agg, auto_agg_name),
            auto_agg_name,
        );
    }

    // หากชื่อคอลัมน์ไปซ้ำกับตารางอื่นที่ Join มา ให้เติม Alias ตารางนำหน้าอัตโนมัติ (เช่น vhd_RecId, vdt_RecId)
    let count = name_counts
        .get(&field.column.column_name.to_lowercase())
        .copied()
        .unwrap_or(0);

    if count > 1 {
        let disambiguated = format!("{}_{}", field.column.table_alias, field.column.column_name);
        (
            format!("{} AS [{}]", expr_with_agg, disambiguated),
            disambiguated,
        )
    } else {
        (expr_with_agg, field.column.column_name.clone())
    }
}

fn format_sql_literal(raw_value: &str, is_numeric: bool, operator: &CompareOperator) -> String {
    if *operator == CompareOperator::In {
        let items: Vec<String> = raw_value
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                if is_numeric {
                    s.to_string()
                } else {
                    format!("N'{}'", s.replace('\'', "''"))
                }
            })
            .collect();
        return format!("({})", items.join(", "));
    }

    if is_numeric {
        raw_value.trim().to_string()
    } else {
        let escaped = raw_value.replace('\'', "''");
        if *operator == CompareOperator::Like && !escaped.contains('%') {
            format!("N'%{}%'", escaped)
        } else {
            format!("N'{}'", escaped)
        }
    }
}
