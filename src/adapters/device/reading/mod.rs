//! 分页与批量读取游戏状态，协调完整快照采集。

pub(in crate::adapters::device) mod collections;
pub mod equipment;
pub(in crate::adapters::device) mod game_state;
pub mod ship_catalog;
