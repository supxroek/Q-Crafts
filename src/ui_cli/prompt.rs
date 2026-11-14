use anyhow::Result;
use dialoguer::{Confirm, FuzzySelect, Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use qcraft::config::app_config::{AppConfig, AuthMode};
use qcraft::config::dictionary::SchemaDictionary;
use qcraft::core::exporters::ExportTarget;
use qcraft::core::query_ast::{
    AggregateFunc, ColumnRef, CompareOperator, FilterValue, JoinClause, JoinType, OrderField,
    QueryAst, SelectField, SortDirection, TableRef, WhereCondition,
};
use qcraft::core::schema_graph::{
    ColumnDetail, SqlClient, fetch_related_foreign_keys, fetch_table_details_with_samples,
};
use std::collections::HashSet;

/// โครงสร้างช่วยเก็บข้อมูลตารางและคอลัมน์ทั้งหมดที่ถูกดึงเข้ามาใน Query ปัจจุบัน
pub struct LoadedTableContext {
    pub table_ref: TableRef,
    pub columns: Vec<ColumnDetail>,
}

/// 1. Wizard สำหรับคำสั่ง `conn` (ตั้งค่าการเชื่อมต่อฐานข้อมูล)
pub fn run_conn_wizard(existing: &AppConfig) -> Result<AppConfig> {
    let theme = ColorfulTheme::default();
    println!("\n🔌 ตั้งค่าการเชื่อมต่อฐานข้อมูล SQL Server");

    let auth_options = vec![
        "SQL Server Authentication (Username / Password)",
        "Windows Authentication (Integrated Security)",
    ];
    let default_idx = match existing.auth_mode {
        AuthMode::SqlServer => 0,
        AuthMode::Windows => 1,
    };

    let auth_sel = Select::with_theme(&theme)
        .with_prompt("├── รูปแบบการเชื่อมต่อ (Authentication Mode)")
        .items(&auth_options)
        .default(default_idx)
        .interact()?;

    let auth_mode = if auth_sel == 0 {
        AuthMode::SqlServer
    } else {
        AuthMode::Windows
    };

    let host: String = Input::with_theme(&theme)
        .with_prompt("├── Server Host / Instance (เช่น spc805srv หรือ spc805srv\\SQLEXPRESS)")
        .default(existing.host.clone())
        .interact_text()?;

    let port: u16 = Input::with_theme(&theme)
        .with_prompt("├── Port (ค่าเริ่มต้น 1433)")
        .default(existing.port)
        .interact_text()?;

    let mut db_input = Input::with_theme(&theme).with_prompt("├── ชื่อฐานข้อมูล (Database Name)");
    if !existing.database.is_empty() {
        db_input = db_input.default(existing.database.clone());
    }
    let database: String = db_input.interact_text()?;

    let (username, password) = if auth_mode == AuthMode::SqlServer {
        let user: String = Input::with_theme(&theme)
            .with_prompt("├── Username")
            .default(
                existing
                    .username
                    .clone()
                    .unwrap_or_else(|| "sa".to_string()),
            )
            .interact_text()?;

        let pass: String = Password::with_theme(&theme)
            .with_prompt("├── Password (พิมพ์แล้วจะไม่แสดงตัวอักษร)")
            .allow_empty_password(true)
            .interact()?;

        let final_pass = if pass.is_empty() {
            existing.password.clone()
        } else {
            Some(pass)
        };
        (Some(user), final_pass)
    } else {
        (None, None)
    };

    let preview_limit: u32 = Input::with_theme(&theme)
        .with_prompt("└── จำนวนแถวสูงสุดสำหรับแสดงตัวอย่างข้อมูล (Preview Limit)")
        .default(existing.default_preview_limit)
        .interact_text()?;

    let new_cfg = AppConfig {
        host,
        port,
        database,
        auth_mode,
        username,
        password,
        trust_cert: true,
        default_preview_limit: preview_limit,
        global_rules: existing.global_rules.clone(),
    };

    new_cfg.save()?;
    Ok(new_cfg)
}

/// 2. Wizard หลักสำหรับคำสั่ง `build` (สร้าง Query แบบ Interactive ครบวงจร)
pub async fn run_query_builder_wizard(
    client: &mut SqlClient,
    all_tables: &[String],
    config: &AppConfig,
    dict: &SchemaDictionary,
    initial_table: Option<String>,
) -> Result<QueryAst> {
    let theme = ColorfulTheme::default();
    let mut used_aliases: HashSet<String> = HashSet::new();
    let mut loaded_tables: Vec<LoadedTableContext> = Vec::new();

    println!("\n🔨 qcraft - Interactive Query Builder");

    // -------------------------------------------------------------------------
    // ขั้นที่ 1: เลือกตารางหลัก (Main Table)
    // -------------------------------------------------------------------------
    let main_table_name = if let Some(t) = initial_table {
        println!("├── ตารางหลัก: {}", t);
        t
    } else {
        if all_tables.is_empty() {
            anyhow::bail!("ไม่พบตารางข้อมูลในฐานข้อมูล '{}'", config.database);
        }
        let idx = FuzzySelect::with_theme(&theme)
            .with_prompt("├── เลือกตารางหลัก (พิมพ์ชื่อเพื่อค้นหาได้ทันที)")
            .items(all_tables)
            .default(0)
            .interact()?;
        all_tables[idx].clone()
    };

    let main_alias = generate_unique_alias(&main_table_name, &mut used_aliases);
    print!(
        "│   ⏳ กำลังสแกนโครงสร้างและตัวอย่างข้อมูลของ '{}'...",
        main_table_name
    );
    use std::io::Write;
    std::io::stdout().flush().ok();

    let main_cols = fetch_table_details_with_samples(client, &main_table_name).await?;
    if main_cols.is_empty() {
        anyhow::bail!("ไม่พบคอลัมน์ในตาราง '{}' หรือไม่มีตารางนี้อยู่จริง", main_table_name);
    }
    println!(" พบ {} คอลัมน์ (ชื่อย่อ: {})", main_cols.len(), main_alias);

    let mut ast = QueryAst::new(&main_table_name, &main_alias);
    loaded_tables.push(LoadedTableContext {
        table_ref: ast.main_table.clone(),
        columns: main_cols,
    });

    // -------------------------------------------------------------------------
    // ขั้นที่ 2: เชื่อมตารางอื่น (Auto Foreign Keys + Manual JOIN)
    // -------------------------------------------------------------------------
    let fks = fetch_related_foreign_keys(client, &main_table_name)
        .await
        .unwrap_or_default();
    if !fks.is_empty() {
        println!(
            "├── 🔗 ตรวจพบความสัมพันธ์ Foreign Keys ({} รายการ):",
            fks.len()
        );
        let fk_labels: Vec<String> = fks
            .iter()
            .map(|fk| {
                if fk.from_table.eq_ignore_ascii_case(&main_table_name) {
                    format!(
                        "{} (เชื่อมผ่าน {}.{} = {}.{})",
                        fk.to_table, main_alias, fk.from_column, fk.to_table, fk.to_column
                    )
                } else {
                    format!(
                        "{} (เชื่อมผ่าน {}.{} = {}.{})",
                        fk.from_table, main_alias, fk.to_column, fk.from_table, fk.from_column
                    )
                }
            })
            .collect();

        let selected_fks = MultiSelect::with_theme(&theme)
            .with_prompt("│   เลือกตารางที่ต้องการ JOIN อัตโนมัติ (กด Spacebar เลือก, Enter ข้าม)")
            .items(&fk_labels)
            .interact()?;

        for idx in selected_fks {
            let fk = &fks[idx];
            let (target_tbl, left_col, right_col) =
                if fk.from_table.eq_ignore_ascii_case(&main_table_name) {
                    (
                        fk.to_table.clone(),
                        fk.from_column.clone(),
                        fk.to_column.clone(),
                    )
                } else {
                    (
                        fk.from_table.clone(),
                        fk.to_column.clone(),
                        fk.from_column.clone(),
                    )
                };

            let join_type = prompt_join_type(&theme, &target_tbl)?;
            let target_alias = generate_unique_alias(&target_tbl, &mut used_aliases);

            print!("│   ⏳ กำลังสแกนข้อมูลตัวอย่างของ '{}'...", target_tbl);
            std::io::stdout().flush().ok();
            let target_cols = fetch_table_details_with_samples(client, &target_tbl).await?;
            println!(" สำเร็จ (ชื่อย่อ: {})", target_alias);

            ast.joins.push(JoinClause {
                join_type,
                target_table: TableRef {
                    name: target_tbl.clone(),
                    alias: target_alias.clone(),
                },
                left_column: ColumnRef::new(&main_alias, &left_col),
                right_column: ColumnRef::new(&target_alias, &right_col),
            });

            loaded_tables.push(LoadedTableContext {
                table_ref: TableRef {
                    name: target_tbl,
                    alias: target_alias,
                },
                columns: target_cols,
            });
        }
    }

    // เปิดโอกาสให้เพิ่ม Manual JOIN (กรณีฐานข้อมูลไม่ได้สร้าง Foreign Key ไว้)
    while Confirm::with_theme(&theme)
        .with_prompt("├── ต้องการเพิ่มการ JOIN กับตารางอื่นด้วยตัวเองอีกไหม?")
        .default(false)
        .interact()?
    {
        let target_idx = FuzzySelect::with_theme(&theme)
            .with_prompt("│   เลือกตารางที่ต้องการนำมา JOIN (พิมพ์ค้นหาได้)")
            .items(all_tables)
            .interact()?;
        let target_tbl = all_tables[target_idx].clone();
        let join_type = prompt_join_type(&theme, &target_tbl)?;
        let target_alias = generate_unique_alias(&target_tbl, &mut used_aliases);

        let target_cols = fetch_table_details_with_samples(client, &target_tbl).await?;

        // เลือกคอลัมน์ฝั่งตารางที่มีอยู่แล้ว
        let existing_col_choices = flatten_column_choices(&loaded_tables, dict);
        let left_idx = FuzzySelect::with_theme(&theme)
            .with_prompt("│   เลือกคอลัมน์ฝั่งตารางหลัก/ที่มีอยู่ (ON Left)")
            .items(&existing_col_choices.0)
            .interact()?;
        let left_col_ref = existing_col_choices.1[left_idx].0.clone();

        // เลือกคอลัมน์ฝั่งตารางใหม่
        let right_col_names: Vec<String> = target_cols
            .iter()
            .map(|c| format!("{}.{} ({})", target_alias, c.name, c.data_type))
            .collect();
        let right_idx = FuzzySelect::with_theme(&theme)
            .with_prompt(format!(
                "│   เลือกคอลัมน์ฝั่งตาราง '{}' ที่ใช้จับคู่ (ON Right)",
                target_tbl
            ))
            .items(&right_col_names)
            .interact()?;
        let right_col_ref = ColumnRef::new(&target_alias, &target_cols[right_idx].name);

        ast.joins.push(JoinClause {
            join_type,
            target_table: TableRef {
                name: target_tbl.clone(),
                alias: target_alias.clone(),
            },
            left_column: left_col_ref,
            right_column: right_col_ref,
        });

        loaded_tables.push(LoadedTableContext {
            table_ref: TableRef {
                name: target_tbl,
                alias: target_alias,
            },
            columns: target_cols,
        });
    }
    // -------------------------------------------------------------------------
    // ขั้นที่ 3: ตรวจสอบและใช้ Smart Global Rules อัตโนมัติ (เช่น IsActive = 1)
    // -------------------------------------------------------------------------
    if !config.global_rules.is_empty() {
        let mut auto_conditions = Vec::new();
        for tbl_ctx in &loaded_tables {
            for col in &tbl_ctx.columns {
                for rule in &config.global_rules {
                    if col.name.eq_ignore_ascii_case(&rule.column_name) {
                        auto_conditions.push(WhereCondition {
                            column: ColumnRef::new(&tbl_ctx.table_ref.alias, &col.name),
                            operator: CompareOperator::Eq,
                            value: FilterValue::Static {
                                raw_value: rule.value.clone(),
                                is_numeric: rule.is_numeric,
                            },
                            is_auto_rule: true,
                        });
                    }
                }
            }
        }
        if !auto_conditions.is_empty() {
            println!("├── 🛡️  ตรวจพบคอลัมน์ที่ตรงกับกฎพื้นฐาน (Global Rules):");
            for c in &auto_conditions {
                if let FilterValue::Static { raw_value, .. } = &c.value {
                    println!(
                        "│      • {}.{} = {}",
                        c.column.table_alias, c.column.column_name, raw_value
                    );
                }
            }
            if Confirm::with_theme(&theme)
                .with_prompt("│   ต้องการเติมเงื่อนไขเหล่านี้ลงใน WHERE อัตโนมัติหรือไม่?")
                .default(true)
                .interact()?
            {
                ast.where_conditions.extend(auto_conditions);
            }
        }
    }
    // -------------------------------------------------------------------------
    // ขั้นที่ 4: เลือกคอลัมน์แสดงผล (พร้อมข้อมูลตัวอย่างและคำอธิบายจาก Dictionary)
    // -------------------------------------------------------------------------
    let (col_labels, col_refs) = flatten_column_choices(&loaded_tables, dict);
    println!("├── 📋 เลือกคอลัมน์ที่ต้องการแสดงผล (กด Spacebar เลือก, Enter ยืนยัน):");
    let selected_col_indices = MultiSelect::with_theme(&theme)
        .with_prompt("│   รายการคอลัมน์ (แสดงชนิดข้อมูล + ตัวอย่างข้อมูลจริง)")
        .items(&col_labels)
        .interact()?;
    if selected_col_indices.is_empty() {
        anyhow::bail!("กรุณาเลือกคอลัมน์อย่างน้อย 1 คอลัมน์");
    }
    for idx in selected_col_indices {
        let (col_ref, detail, suggested_alias) = &col_refs[idx];
        ast.select_columns.push(SelectField {
            column: col_ref.clone(),
            output_alias: suggested_alias.clone(),
            aggregate: AggregateFunc::None,
            sql_data_type: detail.data_type.clone(),
        });
    }
    // -------------------------------------------------------------------------
    // ขั้นที่ 5: โหมดจัดกลุ่มข้อมูลและคำนวณ (GROUP BY & Aggregation) - ทางเลือกเสริม
    // -------------------------------------------------------------------------
    if ast.select_columns.len() > 1
        && Confirm::with_theme(&theme)
            .with_prompt("├── ต้องการใช้ฟังก์ชันคำนวณและจัดกลุ่ม (SUM, COUNT, GROUP BY) หรือไม่?")
            .default(false)
            .interact()?
    {
        let agg_options = vec![
            "ไม่คำนวณ (ใช้จัดกลุ่ม GROUP BY)",
            "SUM (ผลรวม)",
            "COUNT (นับจำนวน)",
            "AVG (ค่าเฉลี่ย)",
            "MAX (สูงสุด)",
            "MIN (ต่ำสุด)",
        ];
        for field in &mut ast.select_columns {
            let sel = Select::with_theme(&theme)
                .with_prompt(format!(
                    "│   กำหนดรูปแบบให้คอลัมน์ '{}'",
                    field.column.full_name()
                ))
                .items(&agg_options)
                .default(0)
                .interact()?;
            match sel {
                0 => ast.group_by.push(field.column.clone()),
                1 => field.aggregate = AggregateFunc::Sum,
                2 => field.aggregate = AggregateFunc::Count,
                3 => field.aggregate = AggregateFunc::Avg,
                4 => field.aggregate = AggregateFunc::Max,
                5 => field.aggregate = AggregateFunc::Min,
                _ => {}
            }
        }
    }
    // -------------------------------------------------------------------------
    // ขั้นที่ 6: เพิ่มเงื่อนไขกรองข้อมูล (WHERE / @Parameters)
    // -------------------------------------------------------------------------
    while Confirm::with_theme(&theme)
        .with_prompt("├── ต้องการเพิ่มเงื่อนไขกรองข้อมูล (WHERE / @Parameter) หรือไม่?")
        .default(false)
        .interact()?
    {
        let col_idx = FuzzySelect::with_theme(&theme)
            .with_prompt("│   เลือกคอลัมน์ที่ต้องการกรอง (พิมพ์ค้นหาได้)")
            .items(&col_labels)
            .interact()?;
        let (target_col, col_detail, _) = &col_refs[col_idx];
        let is_num = is_numeric_sql_type(&col_detail.data_type);
        let ops = vec![
            "= (เท่ากับ)",
            "<> (ไม่เท่ากับ)",
            "> (มากกว่า)",
            ">= (มากกว่าหรือเท่ากับ)",
            "< (น้อยกว่า)",
            "<= (น้อยกว่าหรือเท่ากับ)",
            "LIKE (ค้นหาคำที่คล้ายกัน)",
            "IN (อยู่ในรายการ คั่นด้วยเครื่องหมายจุลภาค ,)",
            "IS NULL (เป็นค่าว่าง)",
            "IS NOT NULL (ไม่เป็นค่าว่าง)",
        ];
        let op_idx = Select::with_theme(&theme)
            .with_prompt("│   เลือกตัวดำเนินการ (Operator)")
            .items(&ops)
            .default(0)
            .interact()?;
        let operator = match op_idx {
            0 => CompareOperator::Eq,
            1 => CompareOperator::NotEq,
            2 => CompareOperator::Gt,
            3 => CompareOperator::Gte,
            4 => CompareOperator::Lt,
            5 => CompareOperator::Lte,
            6 => CompareOperator::Like,
            7 => CompareOperator::In,
            8 => CompareOperator::IsNull,
            _ => CompareOperator::IsNotNull,
        };
        let filter_value = if !operator.requires_value() {
            FilterValue::None
        } else {
            let val_modes = vec![
                "C# / App Parameter (@ParamName) - สำหรับรับค่าจากหน้าจอโปรแกรม",
                "ค่าคงที่ (Static Value) - กำหนดค่าตายตัวลงใน SQL",
            ];
            let mode_idx = Select::with_theme(&theme)
                .with_prompt("│   เลือกรูปแบบของค่าเงื่อนไข")
                .items(&val_modes)
                .default(0)
                .interact()?;
            if mode_idx == 0 {
                let default_param = to_pascal_case(&target_col.column_name);
                let param_name: String = Input::with_theme(&theme)
                    .with_prompt("│   ตั้งชื่อ Parameter (ไม่ต้องพิมพ์ @)")
                    .default(default_param)
                    .interact_text()?;
                let sample_hint = col_detail
                    .sample_values
                    .first()
                    .cloned()
                    .unwrap_or_default();
                let mut sample_input = Input::with_theme(&theme)
                    .with_prompt("│   ระบุค่าทดสอบชั่วคราวสำหรับใช้ตอน Live Preview");
                if !sample_hint.is_empty() {
                    sample_input = sample_input.default(sample_hint);
                }
                let sample_val: String = sample_input.interact_text()?;
                FilterValue::Parameter {
                    param_name,
                    sample_value: Some(sample_val),
                    is_numeric: is_num,
                }
            } else {
                let raw_val: String = Input::with_theme(&theme)
                    .with_prompt("│   ระบุค่าคงที่ (ไม่ต้องใส่เครื่องหมาย ' ')")
                    .interact_text()?;
                FilterValue::Static {
                    raw_value: raw_val,
                    is_numeric: is_num,
                }
            }
        };
        ast.where_conditions.push(WhereCondition {
            column: target_col.clone(),
            operator,
            value: filter_value,
            is_auto_rule: false,
        });
    }
    // -------------------------------------------------------------------------
    // ขั้นที่ 7: การจัดเรียงลำดับ (ORDER BY)
    // -------------------------------------------------------------------------
    if Confirm::with_theme(&theme)
        .with_prompt("└── ต้องการกำหนดการจัดเรียงข้อมูล (ORDER BY) หรือไม่?")
        .default(false)
        .interact()?
    {
        let col_idx = FuzzySelect::with_theme(&theme)
            .with_prompt("    เลือกคอลัมน์ที่ใช้เรียงลำดับ")
            .items(&col_labels)
            .interact()?;
        let (order_col, _, _) = &col_refs[col_idx];
        let dirs = vec!["DESC (มากไปน้อย / ล่าสุดขึ้นก่อน)", "ASC (น้อยไปมาก / A-Z)"];
        let dir_idx = Select::with_theme(&theme)
            .with_prompt("    เลือกรูปแบบการเรียงลำดับ")
            .items(&dirs)
            .default(0)
            .interact()?;
        ast.order_by.push(OrderField {
            column: order_col.clone(),
            direction: if dir_idx == 0 {
                SortDirection::Desc
            } else {
                SortDirection::Asc
            },
        });
    }
    Ok(ast)
}
/// ถามรูปแบบภาษาเป้าหมายที่ต้องการ Export โค้ด
pub fn prompt_export_target() -> Result<ExportTarget> {
    let theme = ColorfulTheme::default();
    let targets = vec![
        "C# ADO.NET (พร้อม SqlParameter[] สำหรับ WinForms / DbHelper)",
        "Pure T-SQL (คำสั่ง SQL มาตรฐาน ใช้ได้กับทุกโปรแกรม/ฐานข้อมูล)",
        "TypeScript (Interface + Query สำหรับงานเว็บ Vue / React / Node.js)",
    ];

    let idx = Select::with_theme(&theme)
        .with_prompt("📤 เลือกรูปแบบโค้ดที่ต้องการส่งออก (Export Target)")
        .items(&targets)
        .default(0)
        .interact()?;

    Ok(match idx {
        0 => ExportTarget::CSharp,
        1 => ExportTarget::PureSql,
        _ => ExportTarget::TypeScript,
    })
}

// =========================================================================
// Helper Functions
// =========================================================================

fn prompt_join_type(theme: &ColorfulTheme, table_name: &str) -> Result<JoinType> {
    let options = vec![
        "LEFT JOIN (แนะนำ - ดึงข้อมูลตารางหลักทั้งหมดแม้ตารางรองไม่มีข้อมูล)",
        "INNER JOIN (ดึงเฉพาะข้อมูลที่มีตรงกันทั้งสองตาราง)",
    ];
    let sel = Select::with_theme(theme)
        .with_prompt(format!("│   เลือกรูปแบบการ JOIN กับ '{}'", table_name))
        .items(&options)
        .default(0)
        .interact()?;

    Ok(if sel == 0 {
        JoinType::Left
    } else {
        JoinType::Inner
    })
}

/// สร้างชื่อย่อตารางไม่ให้ซ้ำกัน (เช่น Orders -> o, OrderDetails -> od หรือ o2)
fn generate_unique_alias(table_name: &str, used: &mut HashSet<String>) -> String {
    let base: String = table_name
        .chars()
        .filter(|c| c.is_alphabetic())
        .take(1)
        .collect::<String>()
        .to_lowercase();

    let prefix = if base.is_empty() {
        "t".to_string()
    } else {
        base
    };

    if !used.contains(&prefix) {
        used.insert(prefix.clone());
        return prefix;
    }

    let mut counter = 2;
    loop {
        let candidate = format!("{}{}", prefix, counter);
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        counter += 1;
    }
}

/// รวมคอลัมน์จากทุกตารางที่โหลดมา พร้อมสร้างป้ายกำกับแสดงข้อมูลตัวอย่างและคำอธิบายจาก Dictionary
fn flatten_column_choices(
    tables: &[LoadedTableContext],
    dict: &SchemaDictionary,
) -> (Vec<String>, Vec<(ColumnRef, ColumnDetail, Option<String>)>) {
    let mut labels = Vec::new();
    let mut refs = Vec::new();

    for tbl in tables {
        for col in &tbl.columns {
            let mut label = format!("{}.{} ({})", tbl.table_ref.alias, col.name, col.data_type);

            let mut suggested_alias = None;

            // 1. ตรวจสอบคำอธิบายจาก Custom Data Dictionary
            if let Some(note) = dict.get_note(&tbl.table_ref.name, &col.name) {
                label.push_str(&format!(" [📖 {}]", note.description));
                suggested_alias = note.suggested_alias.clone();
            }

            // 2. แนบตัวอย่างข้อมูลจริงที่ไม่ใช่ NULL ต่อท้าย
            if col.sample_values.is_empty() {
                label.push_str(" ──► ตัวอย่าง: (ว่างทั้งหมด)");
            } else {
                let joined_samples = col
                    .sample_values
                    .iter()
                    .map(|s| format!("\"{}\"", s))
                    .collect::<Vec<_>>()
                    .join(", ");
                label.push_str(&format!(" ──► ตัวอย่าง: {}", joined_samples));
            }

            labels.push(label);
            refs.push((
                ColumnRef::new(&tbl.table_ref.alias, &col.name),
                col.clone(),
                suggested_alias,
            ));
        }
    }

    (labels, refs)
}

fn is_numeric_sql_type(sql_type: &str) -> bool {
    matches!(
        sql_type.to_lowercase().as_str(),
        "int"
            | "bigint"
            | "smallint"
            | "tinyint"
            | "bit"
            | "decimal"
            | "numeric"
            | "money"
            | "smallmoney"
            | "float"
            | "real"
    )
}

fn to_pascal_case(input: &str) -> String {
    let mut res = String::new();
    let mut cap = true;
    for c in input.chars() {
        if c == '_' || c == '-' || c == ' ' {
            cap = true;
        } else if cap {
            res.extend(c.to_uppercase());
            cap = false;
        } else {
            res.push(c);
        }
    }
    res
}
