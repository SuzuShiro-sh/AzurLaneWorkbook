//! 定义工作簿目录和受控选择后的稳定应用报告。

use serde::Serialize;

/// 工作簿目录报告的稳定 JSON 外壳版本。
pub const WORKBOOK_CATALOG_SCHEMA_VERSION: u32 = 1;

/// 应用服务使用的工作簿目录相对路径。
pub const WORKBOOK_DIRECTORY: &str = "data/workbooks";

/// 一份可供界面展示的受控工作簿目录项。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookCatalogEntry {
    name: String,
    relative_path: String,
    size_bytes: u64,
    kind: &'static str,
}

impl WorkbookCatalogEntry {
    /// 从已经通过路径边界校验的工作簿元数据建立目录项。
    pub(crate) fn new(name: String, relative_path: String, size_bytes: u64) -> Self {
        // 类型只描述工具保留文件的用途，不代表内容已通过计划校验。
        let kind = match name.to_ascii_lowercase().as_str() {
            "layout-preview.xlsx" => "layout_preview",
            "workbook-layout.updated.xlsx" => "layout_upgrade",
            _ => "workbook",
        };
        Self {
            name,
            relative_path,
            size_bytes,
            kind,
        }
    }

    /// 返回可传给工作簿选择用例的文件名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回工具根目录内的稳定相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回目录读取时确认的普通文件字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回文件用途；工作簿内容是否可执行仍由计划检查确定。
    pub const fn kind(&self) -> &'static str {
        self.kind
    }
}

/// 工作簿目录读取完成后供 CLI 和 GUI 消费的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookCatalogReport {
    message: &'static str,
    schema_version: u32,
    directory: &'static str,
    workbooks: Vec<WorkbookCatalogEntry>,
}

impl WorkbookCatalogReport {
    /// 从已经排序且通过路径边界校验的目录项建立报告。
    pub(crate) fn new(workbooks: Vec<WorkbookCatalogEntry>) -> Self {
        Self {
            message: "工作簿目录读取完成",
            schema_version: WORKBOOK_CATALOG_SCHEMA_VERSION,
            directory: WORKBOOK_DIRECTORY,
            workbooks,
        }
    }

    /// 返回面向用户的目录读取结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回工作簿目录报告的稳定版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回被读取的工具目录相对路径。
    pub const fn directory(&self) -> &'static str {
        self.directory
    }

    /// 返回按稳定名称顺序排列的工作簿目录项。
    pub fn workbooks(&self) -> &[WorkbookCatalogEntry] {
        &self.workbooks
    }

    /// 返回目录中工作簿数量。
    pub const fn count(&self) -> usize {
        self.workbooks.len()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{WORKBOOK_CATALOG_SCHEMA_VERSION, WorkbookCatalogEntry, WorkbookCatalogReport};

    #[test]
    fn serializes_a_stable_directory_report() {
        let report = WorkbookCatalogReport::new(vec![WorkbookCatalogEntry::new(
            "plan.xlsx".to_owned(),
            "data/workbooks/plan.xlsx".to_owned(),
            42,
        )]);

        assert_eq!(
            serde_json::to_value(report).unwrap(),
            json!({
                "message": "工作簿目录读取完成",
                "schema_version": WORKBOOK_CATALOG_SCHEMA_VERSION,
                "directory": "data/workbooks",
                "workbooks": [{
                    "name": "plan.xlsx",
                    "relative_path": "data/workbooks/plan.xlsx",
                    "size_bytes": 42,
                    "kind": "workbook",
                }],
            })
        );
    }
}
