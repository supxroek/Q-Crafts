mod ui_cli;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input};
use qcraft::config::app_config::{AppConfig, CONFIG_FILE_NAME};
use qcraft::config::dictionary::{SchemaDictionary, DICT_FILE_NAME};
use qcraft::core::exporters::export_query;
use qcraft::core::previewer::run_live_preview;
use qcraft::core::query_ast::QueryAst;
use qcraft::core::schema_graph::{
    connect, fetch_all_tables, fetch_table_details_with_samples, inspect_column_distribution,
};
use std::fs;
use std::io::Write;
use ui_cli::args::{CliArgs, Commands};
use ui_cli::prompt::{prompt_export_target, run_conn_wizard, run_query_builder_wizard};
use ui_cli::renderer::{
    render_and_copy_export, render_column_inspection, render_preview_table,
};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("\n❌ เกิดข้อผิดพลาด: {:#}", err);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = CliArgs::parse();

    match cli.command {
        // =====================================================================
        // 1. คำสั่ง `conn` (ตั้งค่าการเชื่อมต่อฐานข้อมูล SQL Server)
        // =====================================================================
        Some(Commands::Conn { show }) => {
            let current_cfg = AppConfig::load()?;

            if show {
                if !AppConfig::exists() {
                    println!(
                        "⚠️  ยังไม่มีไฟล์การตั้งค่า ({}) กรุณารัน `qcraft conn` เพื่อตั้งค่าก่อน",
                        CONFIG_FILE_NAME
                    );
                } else {
                    current_cfg.display_summary();
                }
                return Ok(());
            }

            let new_cfg = run_conn_wizard(&current_cfg)?;
            println!("\n✅ บันทึกการตั้งค่าลงไฟล์ '{}' เรียบร้อยแล้ว!", CONFIG_FILE_NAME);
            new_cfg.display_summary();
        }

        // =====================================================================
        // 2. คำสั่ง `build` (สร้าง Query แบบ Interactive + Live Preview + Export)
        // =====================================================================
        Some(Commands::Build { table, preset }) => {
            let config = ensure_config_ready()?;
            let dict = SchemaDictionary::load_or_create_template()?;

            print!(
                "⏳ กำลังเชื่อมต่อไปยังฐานข้อมูล '{}' ({}:{})... ",
                config.database, config.host, config.port
            );
            std::io::stdout().flush().ok();

            let mut client = connect(&config)
                .await
                .context("ไม่สามารถเชื่อมต่อฐานข้อมูลได้ กรุณาตรวจสอบการตั้งค่าด้วยคำสั่ง `qcraft conn`")?;
            println!("สำเร็จ!");

            // โหลด AST จากไฟล์ Preset (.json) หรือเริ่มสร้างใหม่ผ่าน Wizard
            let ast: QueryAst = if let Some(preset_path) = preset {
                println!("📂 กำลังโหลดโครงสร้าง Query จากไฟล์ Preset '{}'...", preset_path);
                let json_content = fs::read_to_string(&preset_path)
                    .with_context(|| format!("ไม่สามารถอ่านไฟล์ Preset '{}' ได้", preset_path))?;
                serde_json::from_str(&json_content)
                    .with_context(|| format!("รูปแบบไฟล์ Preset '{}' ไม่ถูกต้อง", preset_path))?
            } else {
                let all_tables = if table.is_none() {
                    fetch_all_tables(&mut client).await?
                } else {
                    vec![]
                };

                run_query_builder_wizard(&mut client, &all_tables, &config, &dict, table).await?
            };

            // รัน Live Preview ดึงข้อมูลจริงจากฐานข้อมูลมาแสดงตัวอย่าง
            print!("⏳ กำลังดึงข้อมูลจริงเพื่อแสดงตัวอย่าง (Live Preview)... ");
            std::io::stdout().flush().ok();

            let preview = run_live_preview(&mut client, &ast, config.default_preview_limit).await?;
            println!("สำเร็จ!");
            render_preview_table(&preview);

            // เลือกภาษาเป้าหมายและส่งออกโค้ด (พร้อมคัดลอกลง Clipboard อัตโนมัติ)
            let target = prompt_export_target()?;
            let target_label = match target {
                qcraft::core::exporters::ExportTarget::CSharp => "C# ADO.NET / WinForms",
                qcraft::core::exporters::ExportTarget::PureSql => "Pure T-SQL",
                qcraft::core::exporters::ExportTarget::TypeScript => "TypeScript / Web",
            };

            let exported_code = export_query(&ast, target);
            render_and_copy_export(&exported_code, target_label);

            // ถามบันทึกโครงสร้าง Query นี้เก็บเป็น Preset (.json) ไว้ใช้ซ้ำในอนาคตหรือไม่
            let theme = ColorfulTheme::default();
            if Confirm::with_theme(&theme)
                .with_prompt("💾 ต้องการบันทึกโครงสร้าง Query นี้เป็นไฟล์ Preset (.json) ไว้ใช้ซ้ำหรือไม่?")
                .default(false)
                .interact()?
            {
                let default_filename = format!("query_{}.json", ast.main_table.name.to_lowercase());
                let file_name: String = Input::with_theme(&theme)
                    .with_prompt("   ตั้งชื่อไฟล์ Preset")
                    .default(default_filename)
                    .interact_text()?;

                let json_str = serde_json::to_string_pretty(&ast)?;
                fs::write(&file_name, json_str)
                    .with_context(|| format!("ไม่สามารถบันทึกไฟล์ '{}' ได้", file_name))?;

                println!(
                    "✅ บันทึก Preset สำเร็จ! ครั้งต่อไปสามารถเรียกใช้ได้ทันทีด้วยคำสั่ง:\n   qcraft build --preset {}\n",
                    file_name
                );
            }
        }

        // =====================================================================
        // 3. คำสั่ง `inspect` (เจาะลึกข้อมูลตัวอย่างและสถิติความถี่ของคอลัมน์)
        // =====================================================================
        Some(Commands::Inspect { table, column }) => {
            let config = ensure_config_ready()?;
            let theme = ColorfulTheme::default();

            print!("⏳ กำลังเชื่อมต่อไปยังฐานข้อมูล '{}'... ", config.database);
            std::io::stdout().flush().ok();
            let mut client = connect(&config).await?;
            println!("สำเร็จ!\n");

            let target_table = if let Some(t) = table {
                t
            } else {
                let tables = fetch_all_tables(&mut client).await?;
                let idx = FuzzySelect::with_theme(&theme)
                    .with_prompt("เลือกตารางที่ต้องการตรวจสอบ (พิมพ์ค้นหาได้)")
                    .items(&tables)
                    .interact()?;
                tables[idx].clone()
            };

            let target_column = if let Some(c) = column {
                c
            } else {
                let cols = fetch_table_details_with_samples(&mut client, &target_table).await?;
                let col_names: Vec<String> = cols
                    .iter()
                    .map(|c| format!("{} ({})", c.name, c.data_type))
                    .collect();
                let idx = FuzzySelect::with_theme(&theme)
                    .with_prompt(format!("เลือกคอลัมน์ในตาราง '{}' ที่ต้องการเจาะลึก", target_table))
                    .items(&col_names)
                    .interact()?;
                cols[idx].name.clone()
            };

            let stats = inspect_column_distribution(&mut client, &target_table, &target_column).await?;
            render_column_inspection(&target_table, &target_column, &stats);
        }

        // =====================================================================
        // 4. คำสั่ง `dict` (จัดการไฟล์พจนานุกรมคำอธิบายคอลัมน์ส่วนตัว)
        // =====================================================================
        Some(Commands::Dict) => {
            let dict = SchemaDictionary::load_or_create_template()?;
            println!("📖 ไฟล์พจนานุกรมคำอธิบายคอลัมน์ส่วนตัว: '{}'", DICT_FILE_NAME);
            println!("   ├── จำนวนตารางที่บันทึกคำอธิบายไว้: {} ตาราง", dict.tables.len());
            println!("   └── คุณสามารถเปิดไฟล์ '{}' ด้วย Notepad หรือ VS Code เพื่อเพิ่มคำอธิบายภาษาไทยให้คอลัมน์ที่ชื่อไม่สื่อความหมายได้ทันที\n", DICT_FILE_NAME);
        }

        // =====================================================================
        // กรณีรันโปรแกรมโดยไม่ใส่คำสั่งย่อย -> แสดงหน้าจอช่วยเหลือ (--help)
        // =====================================================================
        None => {
            CliArgs::command().print_help()?;
            println!("\n\n💡 คำแนะนำ: เริ่มต้นใช้งานครั้งแรกด้วยการพิมพ์ `qcraft conn` และสร้างคิวรีด้วย `qcraft build`");
        }
    }

    Ok(())
}

/// ตรวจสอบว่ามีไฟล์ตั้งค่าฐานข้อมูลหรือยัง หากไม่มีให้พาไปตั้งค่าอัตโนมัติ
fn ensure_config_ready() -> Result<AppConfig> {
    if !AppConfig::exists() {
        println!(
            "⚠️  ไม่พบไฟล์การตั้งค่า ({}) ระบบจะพาเข้าสู่ขั้นตอนตั้งค่าเบื้องต้นก่อน...",
            CONFIG_FILE_NAME
        );
        let default_cfg = AppConfig::default();
        let new_cfg = run_conn_wizard(&default_cfg)?;
        println!("\n✅ ตั้งค่าสำเร็จ! กำลังดำเนินการต่อ...\n");
        Ok(new_cfg)
    } else {
        AppConfig::load()
    }
}
