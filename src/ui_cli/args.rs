use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "qcraft",
    version = "0.2.3",
    author = "FullStack & WinForms Dev",
    about = "🔨 เครื่องมือสร้าง SQL Query อัจฉริยะ พร้อมระบบพรีวิวข้อมูลจริงและส่งออกโค้ดหลายภาษา",
    long_about = "qcraft ช่วยลดเวลาการเขียนและทดสอบ SQL Query สำหรับฐานข้อมูลองค์กรขนาดใหญ่\nรองรับการสแกนข้อมูลตัวอย่างอัตโนมัติ (Non-Null Scan), แนะนำการ JOIN, พรีวิวผลลัพธ์จริง และส่งออกเป็น Pure SQL, C# และ TypeScript"
)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// 🔌 ตั้งค่าและบันทึกการเชื่อมต่อฐานข้อมูล SQL Server
    #[command(visible_alias = "config")]
    Conn {
        /// แสดงรายละเอียดการตั้งค่าปัจจุบัน (ซ่อนรหัสผ่าน)
        #[arg(short, long)]
        show: bool,
    },

    /// 🔨 เริ่มสร้าง SQL Query แบบ Interactive (เลือกตาราง, Join, คอลัมน์, เงื่อนไข และพรีวิวข้อมูล)
    #[command(visible_alias = "create")]
    Build {
        /// ระบุชื่อตารางหลักทันที (หากไม่ใส่ จะแสดงรายการตารางทั้งหมดให้ค้นหาและเลือก)
        #[arg(short, long)]
        table: Option<String>,

        /// โหลดโครงสร้าง Query เดิมจากไฟล์ Preset (.json) เพื่อนำมาแก้ไขหรือรันใหม่
        #[arg(short, long)]
        preset: Option<String>,
    },

    /// 🔍 เจาะลึกดูข้อมูลตัวอย่างและสถิติความถี่ (DISTINCT + COUNT) ของคอลัมน์ที่สงสัย
    Inspect {
        /// ชื่อตารางที่ต้องการตรวจสอบ (หากไม่ใส่ จะให้เลือกจากรายการ)
        #[arg(short, long)]
        table: Option<String>,

        /// ชื่อคอลัมน์ที่ต้องการดูข้อมูล (หากไม่ใส่ จะให้เลือกจากรายการ)
        #[arg(short, long)]
        column: Option<String>,
    },

    /// 📖 สร้างหรือตรวจสอบไฟล์พจนานุกรมคำอธิบายคอลัมน์ส่วนตัว (qcraft_dict.toml)
    Dict,
}
