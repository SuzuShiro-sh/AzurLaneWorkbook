//! 有界 XLSX 包读取、OOXML 定点编辑与原子发布。
mod error;
pub use error::XlsxError;
pub mod atomic;
pub mod editor;
pub mod limits;
pub mod package;
pub mod paths;
pub mod workbook;
pub mod worksheet_primitives;
