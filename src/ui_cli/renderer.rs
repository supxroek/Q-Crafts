use arboard::Clipboard;
use comfy_table::{presets::UTF8_FULL, Cell, Color, ContentArrangement, Table};
use qcraft::core::previewer::PreviewResult;

/// วาดตารางแสดงผลลัพธ์ข้อมูลจริง (Live Preview) ในหน้าจอ Terminal แบบฉลาด
pub fn render_preview_table(preview: &PreviewResult) {
    println!("\n📊 ผลลัพธ์ข้อมูลจริงจากฐานข้อมูล (แสดงตัวอย่าง {} แถว, {} คอลัมน์):", preview.rows.len(), preview.headers.len());

    if preview.rows.is_empty() {
        println!("   ⚠️  ไม่พบข้อมูลที่ตรงกับเงื่อนไขนี้ (0 แถว)");
        println!("   💡 คำแนะนำ: ลองตรวจสอบเงื่อนไข WHERE หรือค่าทดสอบของ @Parameter อีกครั้ง\n");
        return;
    }

    // กำหนดเกณฑ์: หากคอลัมน์เยอะเกินไป (เช่น > 6 คอลัมน์) ให้จำกัดการแสดงผลหรือจัดระเบียบ
    const MAX_VISIBLE_COLUMNS: usize = 6;
    let total_cols = preview.headers.len();

    let (headers_to_render, rows_to_render, is_truncated) = if total_cols > MAX_VISIBLE_COLUMNS {
        // ตัดแสดงเฉพาะ 6 คอลัมน์แรก เพื่อไม่ให้ตารางพังยับเยินบนหน้าจอแคบ
        let h = preview.headers[..MAX_VISIBLE_COLUMNS].to_vec();
        let r = preview.rows.iter().map(|row| row[..MAX_VISIBLE_COLUMNS].to_vec()).collect();
        (h, r, true)
    } else {
        (preview.headers.clone(), preview.rows.clone(), false)
    };

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    // 1. สร้างแถวหัวตาราง (Headers)
    let header_cells: Vec<Cell> = headers_to_render
        .iter()
        .map(|h| Cell::new(h).fg(Color::Cyan))
        .collect();
    table.set_header(header_cells);

    // 2. เพิ่มข้อมูลทีละแถว (ไฮไลต์ค่า NULL เป็นสีเทาเข้ม)
    for row in &rows_to_render {
        let cells: Vec<Cell> = row
            .iter()
            .map(|val| {
                if val == "NULL" {
                    Cell::new("NULL").fg(Color::DarkGrey)
                } else {
                    Cell::new(val)
                }
            })
            .collect();
        table.add_row(cells);
    }

    println!("{table}");

    if is_truncated {
        println!(
            "   ⚠️  หมายเหตุ: ตารางนี้มีทั้งหมด {} คอลัมน์ แต่แสดงให้เห็นก่อน {} คอลัมน์แรกเพื่อความสะอาดในการอ่าน",
            total_cols, MAX_VISIBLE_COLUMNS
        );
        println!("   💡 คำแนะนำ: หากต้องการดูทุกคอลัมน์ สามารถเลือกเฉพาะคอลัมน์ที่สนใจในเมนู (SELECT Columns) ได้ครับ\n");
    } else {
        println!();
    }
}

/// วาดตารางแสดงสถิติข้อมูลของคอลัมน์เดียว (สำหรับคำสั่ง `inspect`)
pub fn render_column_inspection(table_name: &str, column_name: &str, stats: &[(String, i32)]) {
    println!(
        "\n🔍 ผลการตรวจสอบข้อมูลในคอลัมน์ [{}.{}] (10 อันดับที่พบบ่อยที่สุด):",
        table_name, column_name
    );

    if stats.is_empty() {
        println!("   ⚠️  ตารางนี้ยังไม่มีข้อมูล (0 แถว)\n");
        return;
    }

    let total_sampled: i32 = stats.iter().map(|(_, cnt)| *cnt).sum();

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ค่าข้อมูลที่เก็บอยู่ (Value)").fg(Color::Cyan),
            Cell::new("จำนวนที่พบ (Rows)").fg(Color::Cyan),
            Cell::new("สัดส่วน (%)").fg(Color::Cyan),
        ]);

    for (val, count) in stats {
        let pct = if total_sampled > 0 {
            (*count as f64 / total_sampled as f64) * 100.0
        } else {
            0.0
        };

        let val_cell = if val == "NULL" {
            Cell::new("NULL").fg(Color::DarkGrey)
        } else {
            Cell::new(val).fg(Color::Green)
        };

        table.add_row(vec![
            val_cell,
            Cell::new(count.to_string()),
            Cell::new(format!("{:.1}%", pct)),
        ]);
    }

    println!("{table}\n");
}

/// แสดงผลโค้ดที่ส่งออก และคัดลอกลง Clipboard ของเครื่องให้อัตโนมัติ
pub fn render_and_copy_export(code: &str, target_label: &str) {
    println!("\n🚀 โค้ดพร้อมใช้งาน ({}):", target_label);
    println!("──────────────────────────────────────────────────────────────────────────");
    println!("{}", code);
    println!("──────────────────────────────────────────────────────────────────────────");

    // คัดลอกลง Clipboard อัตโนมัติด้วย arboard
    match Clipboard::new().and_then(|mut cb| cb.set_text(code.to_string())) {
        Ok(_) => println!("📋 คัดลอกโค้ดลง Clipboard เรียบร้อยแล้ว! (กด Ctrl+V วางในโปรเจกต์ได้ทันที)\n"),
        Err(e) => println!("⚠️  ไม่สามารถคัดลอกลง Clipboard อัตโนมัติได้ ({}) กรุณาก๊อปปี้จากด้านบน\n", e),
    }
}
