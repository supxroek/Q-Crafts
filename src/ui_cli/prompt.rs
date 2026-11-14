use crate::ui_cli::renderer::render_preview_table;
use anyhow::Result;
use dialoguer::{Confirm, FuzzySelect, Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use qcraft::config::app_config::{AppConfig, AuthMode};
use qcraft::config::dictionary::SchemaDictionary;
use qcraft::core::exporters::ExportTarget;
use qcraft::core::previewer::run_live_preview;
use qcraft::core::query_ast::{
    AggregateFunc, ColumnRef, CompareOperator, FilterValue, JoinClause, JoinType, OrderField,
    QueryAst, SelectField, SortDirection, TableRef, WhereCondition,
};
use qcraft::core::schema_graph::{
    ColumnDetail, SqlClient, fetch_all_databases, fetch_related_foreign_keys,
    fetch_table_details_with_samples, fetch_tables_in_db,
};
use std::collections::HashSet;
use std::io::Write;

/// โครงสร้างช่วยเก็บข้อมูลตารางและคอลัมน์ทั้งหมดที่ถูกโหลดเข้ามาใน Query ปัจจุบัน
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

    let mut db_input =
        Input::with_theme(&theme).with_prompt("├── ชื่อฐานข้อมูลเริ่มต้น (Default Database)");
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

/// 2. [อัปเกรด v0.2] Interactive Query Builder Hub (รองรับข้าม Database, แก้ไขย้อนกลับ และ Live Preview ซ้ำได้ไม่จำกัด)
pub async fn run_query_builder_hub(
    client: &mut SqlClient,
    config: &AppConfig,
    dict: &SchemaDictionary,
    initial_table: Option<String>,
) -> Result<Option<QueryAst>> {
    let theme = ColorfulTheme::default();
    let mut used_aliases: HashSet<String> = HashSet::new();
    let mut loaded_tables: Vec<LoadedTableContext> = Vec::new();

    println!("\n🔨 QCraft v0.2 - Interactive Query Builder");

    // -------------------------------------------------------------------------
    // ขั้นเริ่มต้น: เลือกฐานข้อมูลและตารางหลัก (Main Table) - แก้ไข Type กำกับให้ชัดเจนป้องกัน E0308
    // -------------------------------------------------------------------------
    let (main_db, main_schema, main_table_name): (String, String, String) =
        if let Some(t) = initial_table {
            let db = if config.database.is_empty() {
                prompt_select_database_async(client, &theme, None).await?
            } else {
                config.database.clone()
            };
            (db, "dbo".to_string(), t)
        } else {
            let default_db = if config.database.is_empty() {
                None
            } else {
                Some(config.database.as_str())
            };
            let selected_db = prompt_select_database_async(client, &theme, default_db).await?;
            let tables_in_db = fetch_tables_in_db(client, &selected_db).await?;

            if tables_in_db.is_empty() {
                anyhow::bail!("ไม่พบตารางข้อมูลในฐานข้อมูล '{}'", selected_db);
            }

            let table_labels: Vec<String> = tables_in_db.iter().map(|t| t.display_name()).collect();
            let idx = FuzzySelect::with_theme(&theme)
                .with_prompt(format!(
                    "├── เลือกตารางหลักในฐานข้อมูล '{}' (พิมพ์ชื่อเพื่อค้นหา)",
                    selected_db
                ))
                .items(&table_labels)
                .default(0)
                .interact()?;

            let chosen = &tables_in_db[idx];
            (selected_db, chosen.schema.clone(), chosen.name.clone())
        };

    // สร้าง Smart Semantic Alias ที่ฉลาดขึ้น
    let suggested_alias = generate_smart_alias(&main_table_name, &mut used_aliases);
    let main_alias: String = Input::with_theme(&theme)
        .with_prompt(format!(
            "├── ตั้งชื่อย่อ (Alias) สำหรับตาราง '{}'",
            main_table_name
        ))
        .default(suggested_alias.clone())
        .interact_text()?;

    if main_alias != suggested_alias {
        used_aliases.remove(&suggested_alias);
        used_aliases.insert(main_alias.clone());
    }

    print!(
        "│   ⏳ กำลังสแกนโครงสร้างและตัวอย่างข้อมูลของ '{}.{}.{}'...",
        main_db, main_schema, main_table_name
    );
    std::io::stdout().flush().ok();

    let main_cols =
        fetch_table_details_with_samples(client, &main_db, &main_schema, &main_table_name).await?;
    if main_cols.is_empty() {
        anyhow::bail!("ไม่พบคอลัมน์ในตาราง '{}'", main_table_name);
    }
    println!(" พบ {} คอลัมน์!", main_cols.len());

    let mut ast = QueryAst::new(&main_db, &main_schema, &main_table_name, &main_alias);
    loaded_tables.push(LoadedTableContext {
        table_ref: ast.main_table.clone(),
        columns: main_cols,
    });

    // ตรวจสอบ Smart Global Rules อัตโนมัติสำหรับตารางหลัก
    apply_global_rules_for_table(&theme, config, &loaded_tables, &mut ast)?;

    // -------------------------------------------------------------------------
    // เข้าสู่ State-Machine Menu Loop
    // -------------------------------------------------------------------------
    loop {
        display_current_query_tree(&ast);

        let menu_items = vec![
            "1. 🔗 จัดการการเชื่อมตาราง (เพิ่ม / ลบ JOIN - รองรับข้าม Database)",
            "2. 📋 เลือก / แก้ไข คอลัมน์แสดงผล (SELECT Columns)",
            "3. 🧮 กำหนดการจัดกลุ่มและคำนวณ (GROUP BY / SUM / COUNT)",
            "4. 🔍 เพิ่ม / ลบ เงื่อนไขกรองข้อมูล (WHERE / @Parameters)",
            "5. ↕️  กำหนดการจัดเรียงลำดับ (ORDER BY)",
            "6. 📊 ทดสอบดึงข้อมูลจริง (Live Data Preview)",
            "7. 🚀 ยืนยันและส่งออกโค้ด (Export & Copy to Clipboard)",
            "8. ❌ ยกเลิกและออกจากโปรแกรม",
        ];

        let choice = Select::with_theme(&theme)
            .with_prompt("เลือกขั้นตอนที่ต้องการดำเนินการ (สามารถย้อนกลับมาแก้ไขได้ตลอด)")
            .items(&menu_items)
            .default(if ast.select_columns.is_empty() { 1 } else { 5 })
            .interact()?;

        match choice {
            0 => {
                manage_joins_step(
                    client,
                    &theme,
                    config,
                    dict,
                    &mut ast,
                    &mut loaded_tables,
                    &mut used_aliases,
                )
                .await?;
            }
            1 => {
                manage_select_columns_step(&theme, dict, &loaded_tables, &mut ast)?;
            }
            2 => {
                manage_grouping_step(&theme, &mut ast)?;
            }
            3 => {
                manage_where_step(&theme, dict, &loaded_tables, &mut ast)?;
            }
            4 => {
                manage_order_by_step(&theme, dict, &loaded_tables, &mut ast)?;
            }
            5 => {
                print!("⏳ กำลังยิงคิวรีทดสอบดึงข้อมูลจริง... ");
                std::io::stdout().flush().ok();
                match run_live_preview(client, &ast, config.default_preview_limit).await {
                    Ok(preview) => {
                        println!("สำเร็จ!");
                        render_preview_table(&preview);
                    }
                    Err(e) => {
                        println!("ล้มเหลว!");
                        eprintln!("\n❌ ข้อผิดพลาดจากการรันทดสอบ:\n{:#}\n", e);
                    }
                }
            }
            6 => {
                if ast.select_columns.is_empty() {
                    if !Confirm::with_theme(&theme)
                        .with_prompt(
                            "⚠️  คุณยังไม่ได้เลือกคอลัมน์เฉพาะเจาะจง ต้องการส่งออกเป็น `SELECT *` หรือไม่?",
                        )
                        .default(false)
                        .interact()?
                    {
                        continue;
                    }
                }
                return Ok(Some(ast));
            }
            _ => {
                println!("ยกเลิกการสร้าง Query");
                return Ok(None);
            }
        }
    }
}

// =============================================================================
// Sub-Steps สำหรับแต่ละเมนูใน Hub
// =============================================================================

async fn manage_joins_step(
    client: &mut SqlClient,
    theme: &ColorfulTheme,
    config: &AppConfig,
    dict: &SchemaDictionary,
    ast: &mut QueryAst,
    loaded_tables: &mut Vec<LoadedTableContext>,
    used_aliases: &mut HashSet<String>,
) -> Result<()> {
    let mut actions = vec!["➕ เพิ่มการเชื่อมตารางใหม่ (JOIN - รองรับข้าม Database)"];
    if !ast.joins.is_empty() {
        actions.push("🗑️  ลบการเชื่อมตารางที่มีอยู่ออก");
    }
    actions.push("🔙 กลับสู่เมนูหลัก");

    let act = Select::with_theme(theme)
        .with_prompt("จัดการการเชื่อมตาราง (JOIN)")
        .items(&actions)
        .default(0)
        .interact()?;

    if act == actions.len() - 1 {
        return Ok(());
    }

    if !ast.joins.is_empty() && act == 1 {
        let join_labels: Vec<String> = ast
            .joins
            .iter()
            .map(|j| {
                format!(
                    "{} {} ON {} = {}",
                    j.join_type.as_sql(),
                    j.target_table.display_label(),
                    j.left_column.full_name(),
                    j.right_column.full_name()
                )
            })
            .collect();

        let del_idx = Select::with_theme(theme)
            .with_prompt("เลือก JOIN ที่ต้องการลบออก")
            .items(&join_labels)
            .interact()?;

        let removed = ast.joins.remove(del_idx);
        let removed_alias = removed.target_table.alias;

        loaded_tables.retain(|t| t.table_ref.alias != removed_alias);
        used_aliases.remove(&removed_alias);
        ast.select_columns
            .retain(|c| c.column.table_alias != removed_alias);
        ast.where_conditions
            .retain(|w| w.column.table_alias != removed_alias);
        ast.order_by
            .retain(|o| o.column.table_alias != removed_alias);

        println!("🗑️  ลบการเชื่อมตาราง '{}' เรียบร้อยแล้ว", removed_alias);
        return Ok(());
    }

    let target_db =
        prompt_select_database_async(client, theme, Some(&ast.main_table.database)).await?;
    let tables_in_db = fetch_tables_in_db(client, &target_db).await?;
    let table_labels: Vec<String> = tables_in_db.iter().map(|t| t.display_name()).collect();

    let tbl_idx = FuzzySelect::with_theme(theme)
        .with_prompt(format!(
            "เลือกตารางใน '{}' ที่ต้องการนำมา JOIN (พิมพ์ค้นหาได้)",
            target_db
        ))
        .items(&table_labels)
        .interact()?;

    let chosen_tbl = &tables_in_db[tbl_idx];
    let target_schema = chosen_tbl.schema.clone();
    let target_name = chosen_tbl.name.clone();

    let join_type = prompt_join_type(theme, &target_name)?;

    let suggested_alias = generate_smart_alias(&target_name, used_aliases);
    let target_alias: String = Input::with_theme(theme)
        .with_prompt(format!("ตั้งชื่อย่อ (Alias) สำหรับ '{}'", target_name))
        .default(suggested_alias.clone())
        .interact_text()?;

    if target_alias != suggested_alias {
        used_aliases.remove(&suggested_alias);
        used_aliases.insert(target_alias.clone());
    }

    print!(
        "⏳ กำลังสแกนโครงสร้างและตัวอย่างข้อมูลของ '{}.{}.{}'...",
        target_db, target_schema, target_name
    );
    std::io::stdout().flush().ok();
    let target_cols =
        fetch_table_details_with_samples(client, &target_db, &target_schema, &target_name).await?;
    println!(" สำเร็จ!");

    let mut matched_fk = None;
    if target_db.eq_ignore_ascii_case(&ast.main_table.database) {
        if let Ok(fks) = fetch_related_foreign_keys(
            client,
            &target_db,
            &ast.main_table.schema,
            &ast.main_table.name,
        )
        .await
        {
            matched_fk = fks.into_iter().find(|fk| {
                fk.to_table.eq_ignore_ascii_case(&target_name)
                    || fk.from_table.eq_ignore_ascii_case(&target_name)
            });
        }
    }

    let (left_col_ref, right_col_ref) = if let Some(fk) = matched_fk {
        let (l_col, r_col) = if fk.from_table.eq_ignore_ascii_case(&ast.main_table.name) {
            (fk.from_column, fk.to_column)
        } else {
            (fk.to_column, fk.from_column)
        };

        let auto_msg = format!(
            "ตรวจพบ Foreign Key: {}.{} = {}.{} ต้องการใช้เงื่อนไขนี้เลยหรือไม่?",
            ast.main_table.alias, l_col, target_alias, r_col
        );
        if Confirm::with_theme(theme)
            .with_prompt(auto_msg)
            .default(true)
            .interact()?
        {
            (
                ColumnRef::new(&ast.main_table.alias, &l_col),
                ColumnRef::new(&target_alias, &r_col),
            )
        } else {
            prompt_manual_join_columns(
                theme,
                dict,
                loaded_tables,
                &target_alias,
                &target_name,
                &target_cols,
            )?
        }
    } else {
        prompt_manual_join_columns(
            theme,
            dict,
            loaded_tables,
            &target_alias,
            &target_name,
            &target_cols,
        )?
    };

    ast.joins.push(JoinClause {
        join_type,
        target_table: TableRef::new(&target_db, &target_schema, &target_name, &target_alias),
        left_column: left_col_ref,
        right_column: right_col_ref,
    });

    loaded_tables.push(LoadedTableContext {
        table_ref: TableRef::new(&target_db, &target_schema, &target_name, &target_alias),
        columns: target_cols,
    });

    apply_global_rules_for_table(theme, config, loaded_tables, ast)?;

    Ok(())
}

fn prompt_manual_join_columns(
    theme: &ColorfulTheme,
    dict: &SchemaDictionary,
    loaded_tables: &[LoadedTableContext],
    target_alias: &str,
    target_name: &str,
    target_cols: &[ColumnDetail],
) -> Result<(ColumnRef, ColumnRef)> {
    let (left_labels, left_refs) = flatten_column_choices(loaded_tables, dict);
    let left_idx = FuzzySelect::with_theme(theme)
        .with_prompt("ขั้นที่ 1/2: เลือกคอลัมน์ฝั่งตารางที่มีอยู่แล้ว (ON Left)")
        .items(&left_labels)
        .interact()?;

    let chosen_left_ref = left_refs[left_idx].0.clone();
    let chosen_left_label = &left_labels[left_idx];

    println!("\n   📌 คอลัมน์ฝั่งซ้ายที่เลือกไว้: [ {} ]", chosen_left_label);

    let right_labels: Vec<String> = target_cols
        .iter()
        .map(|col| format_single_column_label(target_alias, target_name, col, dict))
        .collect();

    let right_idx = FuzzySelect::with_theme(theme)
        .with_prompt(format!(
            "📌 ฝั่งซ้ายเลือกไว้: [ {} ]\nขั้นที่ 2/2: เลือกคอลัมน์ฝั่ง '{}' ที่จะจับคู่ (ON Right)",
            chosen_left_label, target_name
        ))
        .items(&right_labels)
        .interact()?;

    let chosen_right_ref = ColumnRef::new(target_alias, &target_cols[right_idx].name);
    Ok((chosen_left_ref, chosen_right_ref))
}

fn manage_select_columns_step(
    theme: &ColorfulTheme,
    dict: &SchemaDictionary,
    loaded_tables: &[LoadedTableContext],
    ast: &mut QueryAst,
) -> Result<()> {
    let (col_labels, col_refs) = flatten_column_choices(loaded_tables, dict);

    let defaults: Vec<bool> = col_refs
        .iter()
        .map(|(cref, _, _)| ast.select_columns.iter().any(|s| s.column == *cref))
        .collect();

    let selected_indices = MultiSelect::with_theme(theme)
        .with_prompt("เลือกคอลัมน์ที่ต้องการแสดงผล (กด Spacebar ติ๊กเลือก/ยกเลิก, Enter ยืนยัน)")
        .items(&col_labels)
        .defaults(&defaults)
        .interact()?;

    ast.select_columns.clear();
    for idx in selected_indices {
        let (col_ref, detail, suggested_alias) = &col_refs[idx];
        ast.select_columns.push(SelectField {
            column: col_ref.clone(),
            output_alias: suggested_alias.clone(),
            aggregate: AggregateFunc::None,
            sql_data_type: detail.data_type.clone(),
        });
    }

    Ok(())
}

fn manage_grouping_step(theme: &ColorfulTheme, ast: &mut QueryAst) -> Result<()> {
    if ast.select_columns.is_empty() {
        println!("⚠️  กรุณาเลือกคอลัมน์ในเมนูที่ 2 (SELECT Columns) ก่อนกำหนด GROUP BY");
        return Ok(());
    }

    ast.group_by.clear();
    let agg_options = vec![
        "ไม่คำนวณ (ใช้จัดกลุ่ม GROUP BY)",
        "SUM (ผลรวม)",
        "COUNT (นับจำนวน)",
        "AVG (ค่าเฉลี่ย)",
        "MAX (สูงสุด)",
        "MIN (ต่ำสุด)",
    ];

    for field in &mut ast.select_columns {
        let sel = Select::with_theme(theme)
            .with_prompt(format!("กำหนดรูปแบบให้คอลัมน์ '{}'", field.column.full_name()))
            .items(&agg_options)
            .default(0)
            .interact()?;

        match sel {
            0 => {
                field.aggregate = AggregateFunc::None;
                ast.group_by.push(field.column.clone());
            }
            1 => field.aggregate = AggregateFunc::Sum,
            2 => field.aggregate = AggregateFunc::Count,
            3 => field.aggregate = AggregateFunc::Avg,
            4 => field.aggregate = AggregateFunc::Max,
            5 => field.aggregate = AggregateFunc::Min,
            _ => {}
        }
    }

    Ok(())
}

fn manage_where_step(
    theme: &ColorfulTheme,
    dict: &SchemaDictionary,
    loaded_tables: &[LoadedTableContext],
    ast: &mut QueryAst,
) -> Result<()> {
    let mut actions = vec!["➕ เพิ่มเงื่อนไขกรองข้อมูลใหม่ (WHERE / @Parameter)"];
    if !ast.where_conditions.is_empty() {
        actions.push("🗑️  ลบเงื่อนไขที่มีอยู่ออก");
    }
    actions.push("🔙 กลับสู่เมนูหลัก");

    let act = Select::with_theme(theme)
        .with_prompt("จัดการเงื่อนไขกรองข้อมูล (WHERE)")
        .items(&actions)
        .default(0)
        .interact()?;

    if act == actions.len() - 1 {
        return Ok(());
    }

    if !ast.where_conditions.is_empty() && act == 1 {
        let cond_labels: Vec<String> = ast
            .where_conditions
            .iter()
            .map(|c| {
                format!(
                    "{} {} {:?}",
                    c.column.full_name(),
                    c.operator.as_sql(),
                    c.value
                )
            })
            .collect();

        let del_idx = Select::with_theme(theme)
            .with_prompt("เลือกเงื่อนไขที่ต้องการลบออก")
            .items(&cond_labels)
            .interact()?;

        ast.where_conditions.remove(del_idx);
        return Ok(());
    }

    let (col_labels, col_refs) = flatten_column_choices(loaded_tables, dict);
    let col_idx = FuzzySelect::with_theme(theme)
        .with_prompt("เลือกคอลัมน์ที่ต้องการกรอง (พิมพ์ค้นหาได้)")
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
    let op_idx = Select::with_theme(theme)
        .with_prompt("เลือกตัวดำเนินการ (Operator)")
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
        let mode_idx = Select::with_theme(theme)
            .with_prompt("เลือกรูปแบบของค่าเงื่อนไข")
            .items(&val_modes)
            .default(0)
            .interact()?;

        if mode_idx == 0 {
            let default_param = to_pascal_case(&target_col.column_name);
            let param_name: String = Input::with_theme(theme)
                .with_prompt("ตั้งชื่อ Parameter (ไม่ต้องพิมพ์ @)")
                .default(default_param)
                .interact_text()?;

            let sample_hint = col_detail
                .sample_values
                .first()
                .cloned()
                .unwrap_or_default();
            let mut sample_input =
                Input::with_theme(theme).with_prompt("ระบุค่าทดสอบชั่วคราวสำหรับใช้ตอน Live Preview");
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
            let raw_val: String = Input::with_theme(theme)
                .with_prompt("ระบุค่าคงที่ (ไม่ต้องใส่เครื่องหมาย ' ')")
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
    Ok(())
}

fn manage_order_by_step(
    theme: &ColorfulTheme,
    dict: &SchemaDictionary,
    loaded_tables: &[LoadedTableContext],
    ast: &mut QueryAst,
) -> Result<()> {
    let mut actions = vec!["➕ เพิ่มการจัดเรียงใหม่ (ORDER BY)"];
    if !ast.order_by.is_empty() {
        actions.push("🗑️  ล้างการจัดเรียงทั้งหมด");
    }
    actions.push("🔙 กลับสู่เมนูหลัก");

    let act = Select::with_theme(theme)
        .with_prompt("จัดการการจัดเรียงข้อมูล (ORDER BY)")
        .items(&actions)
        .default(0)
        .interact()?;

    if act == actions.len() - 1 {
        return Ok(());
    }

    if !ast.order_by.is_empty() && act == 1 {
        ast.order_by.clear();
        return Ok(());
    }

    let (col_labels, col_refs) = flatten_column_choices(loaded_tables, dict);
    let col_idx = FuzzySelect::with_theme(theme)
        .with_prompt("เลือกคอลัมน์ที่ใช้เรียงลำดับ")
        .items(&col_labels)
        .interact()?;

    let (order_col, _, _) = &col_refs[col_idx];
    let dirs = vec!["DESC (มากไปน้อย / ล่าสุดขึ้นก่อน)", "ASC (น้อยไปมาก / A-Z)"];
    let dir_idx = Select::with_theme(theme)
        .with_prompt("เลือกรูปแบบการเรียงลำดับ")
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
    Ok(())
}

fn display_current_query_tree(ast: &QueryAst) {
    println!("\n🌳 สถานะโครงสร้าง Query ปัจจุบัน:");
    println!("   ├── Main Table : {}", ast.main_table.display_label());
    if ast.joins.is_empty() {
        println!("   ├── Joins      : (ไม่มี)");
    } else {
        for (i, j) in ast.joins.iter().enumerate() {
            let prefix = if i == 0 {
                "Joins      :"
            } else {
                "            "
            };
            println!(
                "   ├── {} {} {} ON {} = {}",
                prefix,
                j.join_type.as_sql(),
                j.target_table.display_label(),
                j.left_column.full_name(),
                j.right_column.full_name()
            );
        }
    }

    if ast.select_columns.is_empty() {
        println!("   ├── Select     : (ยังไม่เลือก - ค่าเริ่มต้นจะดึงทั้งหมด *)");
    } else {
        let cols: Vec<String> = ast
            .select_columns
            .iter()
            .map(|c| {
                if c.aggregate != AggregateFunc::None {
                    format!("{:?}({})", c.aggregate, c.column.full_name())
                } else {
                    c.column.full_name()
                }
            })
            .collect();
        println!("   ├── Select     : {}", cols.join(", "));
    }

    if ast.where_conditions.is_empty() {
        println!("   ├── Where      : (ไม่มีเงื่อนไข)");
    } else {
        let wheres: Vec<String> = ast
            .where_conditions
            .iter()
            .map(|w| match &w.value {
                FilterValue::Static { raw_value, .. } => {
                    format!(
                        "{} {} {}",
                        w.column.full_name(),
                        w.operator.as_sql(),
                        raw_value
                    )
                }
                FilterValue::Parameter { param_name, .. } => {
                    format!(
                        "{} {} @{}",
                        w.column.full_name(),
                        w.operator.as_sql(),
                        param_name
                    )
                }
                FilterValue::None => format!("{} {}", w.column.full_name(), w.operator.as_sql()),
            })
            .collect();
        println!("   ├── Where      : {}", wheres.join(" AND "));
    }

    if ast.order_by.is_empty() {
        println!("   └── Order By   : (ไม่ระบุ)\n");
    } else {
        let orders: Vec<String> = ast
            .order_by
            .iter()
            .map(|o| format!("{} {}", o.column.full_name(), o.direction.as_sql()))
            .collect();
        println!("   └── Order By   : {}\n", orders.join(", "));
    }
}

async fn prompt_select_database_async(
    client: &mut SqlClient,
    theme: &ColorfulTheme,
    preferred_db: Option<&str>,
) -> Result<String> {
    let dbs = fetch_all_databases(client).await?;
    if dbs.is_empty() {
        anyhow::bail!("ไม่พบฐานข้อมูลที่ออนไลน์อยู่ในเซิร์ฟเวอร์นี้");
    }
    let default_idx = preferred_db
        .and_then(|pref| dbs.iter().position(|d| d.eq_ignore_ascii_case(pref)))
        .unwrap_or(0);
    let idx = FuzzySelect::with_theme(theme)
        .with_prompt("เลือกฐานข้อมูล (Database) ที่ต้องการดึงตาราง")
        .items(&dbs)
        .default(default_idx)
        .interact()?;
    Ok(dbs[idx].clone())
}

pub fn generate_smart_alias(table_name: &str, used: &mut HashSet<String>) -> String {
    let upper = table_name.to_uppercase();
    let cleaned = upper
        .trim_start_matches("TBL_")
        .trim_start_matches("TB_")
        .trim_start_matches("VW_")
        .trim_end_matches("_TEMP")
        .trim_end_matches("_TMP")
        .trim_end_matches("TEMP")
        .trim_end_matches("TMP");
    let mut words: Vec<String> = Vec::new();
    for part in cleaned.split(|c: char| !c.is_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        if part.len() > 2 && part.ends_with("HD") {
            words.push(part[..part.len() - 2].to_string());
            words.push("HD".to_string());
        } else if part.len() > 2 && part.ends_with("DT") {
            words.push(part[..part.len() - 2].to_string());
            words.push("DT".to_string());
        } else {
            words.push(part.to_string());
        }
    }
    if words.len() == 1 {
        let orig_cleaned = table_name
            .trim_end_matches("_TEMP")
            .trim_end_matches("_TMP");
        let mut pascal_words = Vec::new();
        let mut current = String::new();
        for c in orig_cleaned.chars() {
            if c.is_uppercase() && !current.is_empty() {
                pascal_words.push(current.to_uppercase());
                current = String::new();
            }
            if c.is_alphanumeric() {
                current.push(c);
            }
        }
        if !current.is_empty() {
            pascal_words.push(current.to_uppercase());
        }
        if pascal_words.len() > 1 {
            words = pascal_words;
        }
    }
    let candidate_base = if words.len() >= 2 {
        let last = words.last().unwrap();
        if last == "HD" || last == "DT" {
            let prefix: String = words[..words.len() - 1]
                .iter()
                .filter_map(|w| w.chars().next())
                .collect::<String>()
                .to_lowercase();
            format!("{}_{}", prefix, last.to_lowercase())
        } else {
            words
                .iter()
                .filter_map(|w| w.chars().next())
                .take(3)
                .collect::<String>()
                .to_lowercase()
        }
    } else if let Some(first_word) = words.first() {
        let lower = first_word.to_lowercase();
        for len in 1..=lower.len().min(4) {
            let slice = &lower[..len];
            if !used.contains(slice) {
                used.insert(slice.to_string());
                return slice.to_string();
            }
        }
        lower.chars().take(3).collect()
    } else {
        "t".to_string()
    };
    let base = if candidate_base.is_empty() {
        "t".to_string()
    } else {
        candidate_base
    };
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{}{}", base, counter);
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        counter += 1;
    }
}

fn apply_global_rules_for_table(
    theme: &ColorfulTheme,
    config: &AppConfig,
    loaded_tables: &[LoadedTableContext],
    ast: &mut QueryAst,
) -> Result<()> {
    if config.global_rules.is_empty() {
        return Ok(());
    }
    let mut new_conditions = Vec::new();
    for tbl_ctx in loaded_tables {
        for col in &tbl_ctx.columns {
            for rule in &config.global_rules {
                if col.name.eq_ignore_ascii_case(&rule.column_name) {
                    let cref = ColumnRef::new(&tbl_ctx.table_ref.alias, &col.name);
                    if !ast
                        .where_conditions
                        .iter()
                        .any(|w| w.column == cref && w.is_auto_rule)
                    {
                        new_conditions.push(WhereCondition {
                            column: cref,
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
    }
    if !new_conditions.is_empty() {
        println!("├── 🛡️  ตรวจพบคอลัมน์ที่ตรงกับกฎพื้นฐาน (Global Rules):");
        for c in &new_conditions {
            if let FilterValue::Static { raw_value, .. } = &c.value {
                println!(
                    "│      • {}.{} = {}",
                    c.column.table_alias, c.column.column_name, raw_value
                );
            }
        }
        if Confirm::with_theme(theme)
            .with_prompt("│   ต้องการเติมเงื่อนไขเหล่านี้ลงใน WHERE อัตโนมัติหรือไม่?")
            .default(true)
            .interact()?
        {
            ast.where_conditions.extend(new_conditions);
        }
    }
    Ok(())
}

fn prompt_join_type(theme: &ColorfulTheme, table_name: &str) -> Result<JoinType> {
    let options = vec![
        "LEFT JOIN (แนะนำ - ดึงข้อมูลตารางหลักทั้งหมดแม้ตารางรองไม่มีข้อมูล)",
        "INNER JOIN (ดึงเฉพาะข้อมูลที่มีตรงกันทั้งสองตาราง)",
    ];
    let sel = Select::with_theme(theme)
        .with_prompt(format!("เลือกรูปแบบการ JOIN กับ '{}'", table_name))
        .items(&options)
        .default(0)
        .interact()?;
    Ok(if sel == 0 {
        JoinType::Left
    } else {
        JoinType::Inner
    })
}

fn format_single_column_label(
    table_alias: &str,
    table_name: &str,
    col: &ColumnDetail,
    dict: &SchemaDictionary,
) -> String {
    let mut label = format!("{}.{} ({})", table_alias, col.name, col.data_type);
    if let Some(note) = dict.get_note(table_name, &col.name) {
        label.push_str(&format!(" [📖 {}]", note.description));
    }
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
    label
}

fn flatten_column_choices(
    tables: &[LoadedTableContext],
    dict: &SchemaDictionary,
) -> (Vec<String>, Vec<(ColumnRef, ColumnDetail, Option<String>)>) {
    let mut labels = Vec::new();
    let mut refs = Vec::new();
    for tbl in tables {
        for col in &tbl.columns {
            let label =
                format_single_column_label(&tbl.table_ref.alias, &tbl.table_ref.name, col, dict);
            let suggested_alias = dict
                .get_note(&tbl.table_ref.name, &col.name)
                .and_then(|n| n.suggested_alias.clone());
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
