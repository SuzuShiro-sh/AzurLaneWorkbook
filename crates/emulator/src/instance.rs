use super::EmulatorError;
use std::net::SocketAddr;

pub use suzushiro_target_core::TargetState;

/// 厂商响应解析完成后的实例事实，不承载厂商协议字段。
#[derive(Clone, Debug)]
pub struct EmulatorInstance {
    pub adb_host_ip: Option<String>,
    pub adb_port: Option<u16>,
    pub android_version: Option<String>,
    pub available: bool,
    pub index: String,
    pub is_android_started: bool,
    pub is_process_started: bool,
    pub name: String,
}

impl EmulatorInstance {
    /// 进程、Android 和回环端点足以连接；显式异常状态仍拒绝。
    pub fn is_ready(&self) -> bool {
        self.available
            && self.is_process_started
            && self.is_android_started
            && self.serial().is_ok()
    }

    /// 从已就绪实例构造唯一回环序列号并再次校验。
    pub fn serial(&self) -> Result<String, EmulatorError> {
        let host: &str = self
            .adb_host_ip
            .as_deref()
            .ok_or_else(|| EmulatorError::Discovery {
                message: format!("实例 {} 缺少 adb_host_ip", self.index),
            })?;
        let port: u16 = self.adb_port.ok_or_else(|| EmulatorError::Discovery {
            message: format!("实例 {} 缺少 adb_port", self.index),
        })?;
        let serial: String = format!("{host}:{port}");
        validate_serial(&serial)?;
        Ok(serial)
    }

    /// 根据管理器进程、Android 和 ADB 端点信息归一化实例目录状态。
    pub fn catalog_state(&self) -> TargetState {
        if !self.available {
            return TargetState::Unavailable;
        }
        let endpoint_state = match (&self.adb_host_ip, self.adb_port) {
            (None, None) => None,
            (Some(_), Some(_)) => Some(self.serial().is_ok()),
            _ => Some(false),
        };
        if endpoint_state == Some(false) {
            return TargetState::Unavailable;
        }
        if self.is_ready() {
            return TargetState::Ready;
        }
        if self.is_process_started || self.is_android_started {
            return TargetState::Starting;
        }
        TargetState::Stopped
    }

    /// 新版管理器省略版本字段时保留未知事实，不伪造实例版本。
    pub fn android_version_label(&self) -> &str {
        self.android_version.as_deref().unwrap_or("未报告")
    }
}

pub fn validate_instance_index(instance: &str) -> Result<(), EmulatorError> {
    if instance.is_empty()
        || instance.len() > 8
        || !instance.bytes().all(|byte: u8| byte.is_ascii_digit())
    {
        return Err(EmulatorError::InvalidOption {
            field: "instance",
            message: "必须是单个非负十进制实例索引".to_owned(),
        });
    }
    Ok(())
}

pub fn validate_serial(serial: &str) -> Result<SocketAddr, EmulatorError> {
    let address: SocketAddr = serial.parse().map_err(|_| EmulatorError::InvalidOption {
        field: "serial",
        message: "必须是 HOST:PORT 格式的回环地址".to_owned(),
    })?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(EmulatorError::InvalidOption {
            field: "serial",
            message: "只允许非零端口的回环模拟器地址".to_owned(),
        });
    }
    Ok(address)
}
