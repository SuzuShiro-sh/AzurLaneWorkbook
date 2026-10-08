//! 应用工作簿编辑边界与舰船 Wiki 链接策略。
use super::WorkbookProbeError;
use super::package::PackageSnapshot;
use super::ship_wiki::is_ship_wiki_hyperlink;
use std::path::Path;
pub(crate) use suzushiro_xlsx_toolkit::editor::{
    CellCoordinate, compare_packages, reject_macro_content, validate_cell_reference,
    write_package_to_new_file,
};
pub use suzushiro_xlsx_toolkit::editor::{PackagePreservationEvidence, TextCellEditEvidence};
pub fn edit_text_cell_to_new_file(
    source_path: &Path,
    destination_path: &Path,
    sheet_name: &str,
    cell_reference: &str,
    replacement_value: &str,
) -> Result<TextCellEditEvidence, WorkbookProbeError> {
    Ok(suzushiro_xlsx_toolkit::editor::edit_text_cell_to_new_file(
        source_path,
        destination_path,
        sheet_name,
        cell_reference,
        replacement_value,
        is_ship_wiki_hyperlink,
    )?)
}
pub(crate) fn reject_external_content(package: &PackageSnapshot) -> Result<(), WorkbookProbeError> {
    Ok(suzushiro_xlsx_toolkit::editor::reject_external_content(
        package,
        is_ship_wiki_hyperlink,
    )?)
}
