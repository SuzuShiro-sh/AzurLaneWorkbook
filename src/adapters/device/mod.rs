//! 模拟器设备连接和游戏运行态适配器。

mod adb_config;
pub mod bootstrap;
mod capabilities;
mod capture;
#[cfg(target_os = "windows")]
pub(crate) mod emulator_instance_catalog;
#[cfg(any(target_os = "windows", test))]
mod game_port;
mod mapping;
#[cfg(feature = "native-test-runner")]
pub mod native_test_runner;
pub mod portable;
pub mod probe;
pub mod profile;
mod reading;
pub mod runtime;
pub mod session;
#[cfg(test)]
mod test_support;

pub use capture::equipment_sample::EquipmentSampleEvidence;
pub use capture::full_state::FullStateCaptureEvidence;
pub use capture::ship_catalog::ShipCatalogCaptureEvidence;
#[cfg(target_os = "windows")]
pub(crate) use game_port::ConfiguredAgentManagement;
#[cfg(target_os = "windows")]
pub(crate) use game_port::PortableGamePort;
#[cfg(target_os = "windows")]
pub use game_port::{AgentAction, AgentStatusReport, manage_agent};
pub use mapping::equipment as equipment_mapper;
pub use mapping::equipment_detail as equipment_detail_mapper;
pub use mapping::game_state as game_state_mapper;
pub use mapping::ship as ship_mapper;
pub use reading::equipment as equipment_reader;
pub use reading::ship_catalog as ship_catalog_reader;
pub(crate) use suzushiro_adb::parse_revision as parse_adb_revision;
