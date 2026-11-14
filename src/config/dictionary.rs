use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub const DICT_FILE_NAME: &str = "qcraft_dict.toml";

/// รายละเอียดคำอธิบายและชื่อเล่นของคอลัมน์แต่ละตัว
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ColumnNote {
    pub description: String,          // คำอธิบายภาษาไทย เช่น "ชื่อสินค้าหลัก"
    pub suggested_alias: Option<String>, // ชื่อเล่นที่แนะนำให้ใช้ AS เช่น "ProductName"
}

/// โครงสร้างเก็บพจนานุกรมคำอธิบายคอลัมน์ทั้งหมด
/// Key ชั้นนอก = ชื่อตาราง (เช่น "Products")
/// Key ชั้นใน = ชื่อคอลัมน์ (เช่น "description")
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SchemaDictionary {
    #[serde(default)]
    pub tables: HashMap<String, HashMap<String, ColumnNote>>,
}

impl SchemaDictionary {
    /// โหลดพจนานุกรมจากไฟล์ (หากยังไม่มีไฟล์ ให้สร้างไฟล์ตัวอย่างว่างๆ ไว้ให้)
    pub fn load_or_create_template() -> Result<Self> {
        if !Path::new(DICT_FILE_NAME).exists() {
            let empty_dict = Self::default();
            empty_dict.save_with_comments()?;
            return Ok(empty_dict);
        }

        let content = fs::read_to_string(DICT_FILE_NAME)
            .with_context(|| format!("ไม่สามารถอ่านไฟล์พจนานุกรม '{}' ได้", DICT_FILE_NAME))?;

        let dict: SchemaDictionary = toml::from_str(&content)
            .with_context(|| format!("รูปแบบข้อมูลในไฟล์ '{}' ไม่ถูกต้อง", DICT_FILE_NAME))?;

        Ok(dict)
    }

    /// บันทึกไฟล์พจนานุกรมพร้อมคอมเมนต์ตัวอย่างการใช้งาน
    pub fn save_with_comments(&self) -> Result<()> {
        let mut toml_string = toml::to_string_pretty(self).unwrap_or_default();

        if self.tables.is_empty() {
            toml_string.push_str(
                "# ============================================================================\n\
                 # พจนานุกรมคำอธิบายคอลัมน์ส่วนตัว (Custom Data Dictionary)\n\
                 # ใช้สำหรับบันทึกความหมายของคอลัมน์ที่ชื่อไม่สื่อความหมาย เพื่อแสดงเตือนตอนเลือกคอลัมน์\n\
                 # ============================================================================\n\n\
                 # --- ตัวอย่างการเพิ่มคำอธิบาย (เอาเครื่องหมาย # ออกเพื่อใช้งาน) ---\n\
                 # [tables.Products.description]\n\
                 # description = \"ชื่อสินค้า (ดึงจากระบบ ERP)\"\n\
                 # suggested_alias = \"ProductName\"\n\n\
                 # [tables.Departments.desc_th]\n\
                 # description = \"ชื่อแผนกภาษาไทย\"\n\
                 # suggested_alias = \"DepartmentName\"\n",
            );
        }

        fs::write(DICT_FILE_NAME, toml_string)
            .with_context(|| format!("ไม่สามารถบันทึกไฟล์ '{}' ได้", DICT_FILE_NAME))?;

        Ok(())
    }

    /// ค้นหาคำอธิบายของตารางและคอลัมน์ที่ระบุ (ไม่สนตัวพิมพ์เล็ก-ใหญ่)
    pub fn get_note(&self, table_name: &str, column_name: &str) -> Option<&ColumnNote> {
        // ค้นหาแบบ Case-insensitive
        for (tbl_key, cols) in &self.tables {
            if tbl_key.eq_ignore_ascii_case(table_name) {
                for (col_key, note) in cols {
                    if col_key.eq_ignore_ascii_case(column_name) {
                        return Some(note);
                    }
                }
            }
        }
        None
    }
}
