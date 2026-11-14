use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const CONFIG_FILE_NAME: &str = "qcraft.toml";

/// รูปแบบการยืนยันตัวตนสำหรับ SQL Server
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AuthMode {
    SqlServer,
    Windows,
}

impl Default for AuthMode {
    fn default() -> Self {
        AuthMode::SqlServer
    }
}

/// โครงสร้างสำหรับกำหนด "กฎพื้นฐานอัตโนมัติ (Smart Global Rules)"
/// เช่น หากตารางมีคอลัมน์ชื่อนี้ ให้เติมเงื่อนไข WHERE ให้อัตโนมัติ
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalFilterRule {
    pub column_name: String,   // ชื่อคอลัมน์เป้าหมาย (ไม่สนตัวพิมพ์เล็ก-ใหญ่) เช่น "IsActive"
    pub operator: String,      // ตัวดำเนินการ เช่น "=", "<>"
    pub value: String,         // ค่าที่ต้องการเปรียบเทียบ เช่น "1", "0"
    pub is_numeric: bool,      // true = ไม่ใส่เครื่องหมาย ' ' ครอบค่า
}

/// โครงสร้างหลักสำหรับเก็บการตั้งค่าทั้งหมดลงไฟล์ qcraft.toml
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub auth_mode: AuthMode,
    pub username: Option<String>,
    pub password: Option<String>,
    pub trust_cert: bool,
    pub default_preview_limit: u32,

    /// รายการกฎพื้นฐานอัตโนมัติ (เริ่มต้นเป็นค่าว่าง ตามที่กำหนด)
    ///
    /// ตัวอย่างการเพิ่มกฎในไฟล์ qcraft.toml หรือในโค้ด:
    /// [[global_rules]]
    /// column_name = "IsActive"
    /// operator = "="
    /// value = "1"
    /// is_numeric = true
    ///
    /// [[global_rules]]
    /// column_name = "IsDeleted"
    /// operator = "="
    /// value = "0"
    /// is_numeric = true
    #[serde(default)]
    pub global_rules: Vec<GlobalFilterRule>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 1433,
            database: String::new(),
            auth_mode: AuthMode::SqlServer,
            username: Some("sa".to_string()),
            password: None,
            trust_cert: true,
            default_preview_limit: 5,

            // เริ่มต้นยังไม่กำหนด Rules ใดๆ (สามารถเพิ่มในไฟล์ qcraft.toml หรือเปิดคอมเมนต์ด้านล่างได้)
            global_rules: Vec::new(),
            /* ตัวอย่างการเปิดใช้งานในโค้ด:
            global_rules: vec![
                GlobalFilterRule {
                    column_name: "IsActive".to_string(),
                    operator: "=".to_string(),
                    value: "1".to_string(),
                    is_numeric: true,
                },
            ],
            */
        }
    }
}

impl AppConfig {
    pub fn exists() -> bool {
        Path::new(CONFIG_FILE_NAME).exists()
    }

    pub fn load() -> Result<Self> {
        if !Self::exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(CONFIG_FILE_NAME)
            .with_context(|| format!("ไม่สามารถอ่านไฟล์คอนฟิก '{}' ได้", CONFIG_FILE_NAME))?;

        let config: AppConfig = toml::from_str(&content)
            .with_context(|| format!("รูปแบบข้อมูลในไฟล์ '{}' ไม่ถูกต้อง", CONFIG_FILE_NAME))?;

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let mut toml_string = toml::to_string_pretty(self)
            .context("ไม่สามารถแปลงข้อมูลการตั้งค่าเป็นรูปแบบ TOML ได้")?;

        // แนบคอมเมนต์ตัวอย่างการเพิ่ม Global Rules ท้ายไฟล์ qcraft.toml เพื่อให้แก้ไขง่ายในอนาคต
        if self.global_rules.is_empty() {
            toml_string.push_str(
                "\n# --- ตัวอย่างการเพิ่มกฎพื้นฐานอัตโนมัติ (เอาเครื่องหมาย # ออกเพื่อใช้งาน) ---\n\
                 # [[global_rules]]\n\
                 # column_name = \"IsActive\"\n\
                 # operator = \"=\"\n\
                 # value = \"1\"\n\
                 # is_numeric = true\n",
            );
        }

        fs::write(CONFIG_FILE_NAME, toml_string)
            .with_context(|| format!("ไม่สามารถบันทึกไฟล์ '{}' ลงดิสก์ได้", CONFIG_FILE_NAME))?;

        Ok(())
    }

    pub fn display_summary(&self) {
        let auth_text = match self.auth_mode {
            AuthMode::SqlServer => "SQL Server Authentication",
            AuthMode::Windows => "Windows Authentication",
        };

        println!("📄 การตั้งค่าปัจจุบัน ({}):", CONFIG_FILE_NAME);
        println!("   ├── Server       : {}:{}", self.host, self.port);
        println!("   ├── Database     : {}", if self.database.is_empty() { "(ยังไม่ระบุ)" } else { &self.database });
        println!("   ├── Auth Mode    : {}", auth_text);
        println!("   ├── Preview Rows : {} แถว", self.default_preview_limit);
        println!("   └── Global Rules : {} กฎที่เปิดใช้งาน", self.global_rules.len());
    }
}
