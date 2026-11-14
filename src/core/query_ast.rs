use serde::{Deserialize, Serialize};

/// โครงสร้างหลักของ Query AST (Intermediate Representation)
/// เก็บทุกองค์ประกอบของคำสั่ง SQL เพื่อส่งต่อให้ Compiler, Previewer และ Exporters
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QueryAst {
    pub main_table: TableRef,
    pub joins: Vec<JoinClause>,
    pub select_columns: Vec<SelectField>,
    pub where_conditions: Vec<WhereCondition>,
    pub group_by: Vec<ColumnRef>,
    pub order_by: Vec<OrderField>,
    pub top_limit: Option<u32>,
}

impl QueryAst {
    /// สร้าง QueryAst เริ่มต้นจากตารางหลัก
    pub fn new(table_name: &str, alias: &str) -> Self {
        Self {
            main_table: TableRef {
                name: table_name.to_string(),
                alias: alias.to_string(),
            },
            joins: Vec::new(),
            select_columns: Vec::new(),
            where_conditions: Vec::new(),
            group_by: Vec::new(),
            order_by: Vec::new(),
            top_limit: None,
        }
    }
}

/// ข้อมูลอ้างอิงตารางพร้อมชื่อย่อ (เช่น Orders AS o)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TableRef {
    pub name: String,
    pub alias: String,
}

/// คอลัมน์ที่อ้างอิงจากตารางใดตารางหนึ่ง (เช่น o.CustomerId)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ColumnRef {
    pub table_alias: String,
    pub column_name: String,
}

impl ColumnRef {
    pub fn new(table_alias: &str, column_name: &str) -> Self {
        Self {
            table_alias: table_alias.to_string(),
            column_name: column_name.to_string(),
        }
    }

    /// คืนค่ารูปแบบเต็ม เช่น "o.CustomerId"
    pub fn full_name(&self) -> String {
        format!("{}.{}", self.table_alias, self.column_name)
    }
}

// =========================================================================
// 1. ส่วนของการ JOIN ตาราง
// =========================================================================

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
}

impl JoinType {
    pub fn as_sql(&self) -> &'static str {
        match self {
            JoinType::Inner => "INNER JOIN",
            JoinType::Left => "LEFT JOIN",
            JoinType::Right => "RIGHT JOIN",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JoinClause {
    pub join_type: JoinType,
    pub target_table: TableRef,
    pub left_column: ColumnRef,  // คอลัมน์ฝั่งตารางหลัก (เช่น o.CustomerId)
    pub right_column: ColumnRef, // คอลัมน์ฝั่งตารางที่นำมา Join (เช่น c.Id)
}

// =========================================================================
// 2. ส่วนของการเลือกคอลัมน์แสดงผล (SELECT)
// =========================================================================

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AggregateFunc {
    None,
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SelectField {
    pub column: ColumnRef,
    pub output_alias: Option<String>, // ชื่อเล่นคอลัมน์ (AS CustomerName) กรณีชื่อซ้ำหรือใช้ Aggregate
    pub aggregate: AggregateFunc,
    pub sql_data_type: String,        // เก็บ Data Type เดิมไว้ใช้ตอน Export เป็น C# หรือ TypeScript
}

// =========================================================================
// 3. ส่วนของเงื่อนไขกรองข้อมูล (WHERE และ Parameters)
// =========================================================================

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum CompareOperator {
    Eq,         // =
    NotEq,      // <>
    Gt,         // >
    Gte,        // >=
    Lt,         // <
    Lte,        // <=
    Like,       // LIKE '%...%'
    In,         // IN (...)
    IsNull,     // IS NULL
    IsNotNull,  // IS NOT NULL
}

impl CompareOperator {
    pub fn as_sql(&self) -> &'static str {
        match self {
            CompareOperator::Eq => "=",
            CompareOperator::NotEq => "<>",
            CompareOperator::Gt => ">",
            CompareOperator::Gte => ">=",
            CompareOperator::Lt => "<",
            CompareOperator::Lte => "<=",
            CompareOperator::Like => "LIKE",
            CompareOperator::In => "IN",
            CompareOperator::IsNull => "IS NULL",
            CompareOperator::IsNotNull => "IS NOT NULL",
        }
    }

    /// ตรวจสอบว่า Operator นี้ต้องการค่าเปรียบเทียบหรือไม่ (IS NULL ไม่ต้องรับค่า)
    pub fn requires_value(&self) -> bool {
        !matches!(self, CompareOperator::IsNull | CompareOperator::IsNotNull)
    }
}

/// แหล่งที่มาของค่าในเงื่อนไข WHERE
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FilterValue {
    /// ค่าคงที่ตายตัว เช่น '1', 'ACTIVE', '100' (รวมถึงกฎอัตโนมัติอย่าง IsActive = 1)
    Static {
        raw_value: String,
        is_numeric: bool, // true = ไม่ต้องใส่เครื่องหมาย ' ' ครอบ
    },
    /// พารามิเตอร์สำหรับนำไปรับค่าในโค้ดภายนอก เช่น @StartDate
    Parameter {
        param_name: String,         // เช่น "StartDate" (เวลา Export จะเติม @ ให้)
        sample_value: Option<String>, // ค่าชั่วคราวที่ใช้ตอนกด Live Preview
        is_numeric: bool,
    },
    /// กรณีเป็น IS NULL / IS NOT NULL
    None,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WhereCondition {
    pub column: ColumnRef,
    pub operator: CompareOperator,
    pub value: FilterValue,
    pub is_auto_rule: bool, // ระบุว่าเป็นเงื่อนไขที่ระบบเติมให้อัตโนมัติ (เช่น IsActive = 1) หรือไม่
}

// =========================================================================
// 4. ส่วนของการจัดเรียงข้อมูล (ORDER BY)
// =========================================================================

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn as_sql(&self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderField {
    pub column: ColumnRef,
    pub direction: SortDirection,
}
