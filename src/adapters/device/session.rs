//! 保留设备适配器原有调用路径，并转发到独立会话核心模块。

pub use suzushiro_session_core::{
    SESSION_ID_BYTES, SESSION_SECRET_BYTES, SessionGenerationError, SessionId, SessionParseError,
    SessionSecret,
};
