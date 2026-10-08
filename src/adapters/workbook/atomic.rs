//! 应用文本编辑策略与通用原子发布流程的组合。
use super::WorkbookProbeError;
use std::path::Path;
pub use suzushiro_xlsx_toolkit::atomic::AtomicTextCellEditEvidence;
pub(crate) use suzushiro_xlsx_toolkit::atomic::{
    edit_workbook_atomically_with_pre_publish, replace_validated_workbook,
};
pub fn edit_text_cell_atomically(
    workbook_path: &Path,
    sheet_name: &str,
    cell_reference: &str,
    replacement_value: &str,
) -> Result<AtomicTextCellEditEvidence, WorkbookProbeError> {
    Ok(suzushiro_xlsx_toolkit::atomic::edit_text_cell_atomically(
        workbook_path,
        sheet_name,
        cell_reference,
        replacement_value,
        super::ship_wiki::is_ship_wiki_hyperlink,
    )?)
}
