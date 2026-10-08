//! 根据探针请求从归一化实例中选择目标。
#[cfg(target_os = "windows")]
use super::{PortableMode, PortableProbeError, PortableProbeOptions};
#[cfg(target_os = "windows")]
use std::collections::BTreeMap;
#[cfg(target_os = "windows")]
use suzushiro_emulator::{EmulatorInstance, TargetState};

/// 自动模式只在运行实例中选择，避免已停止实例制造无意义歧义。
#[cfg(target_os = "windows")]
pub(super) fn select_instance(
    instances: &BTreeMap<String, EmulatorInstance>,
    options: &PortableProbeOptions,
) -> Result<EmulatorInstance, PortableProbeError> {
    if options.require_ready_instance {
        let index = options
            .instance_hint
            .as_deref()
            .unwrap_or_default()
            .rsplit(':')
            .next()
            .unwrap_or_default();
        return instances
            .get(index)
            .filter(|instance| instance.catalog_state() == TargetState::Ready)
            .cloned()
            .ok_or_else(|| PortableProbeError::Discovery {
                message: format!("所选实例 {index} 不存在或尚未就绪，请刷新信息后重新选择"),
            });
    }
    if options.mode == PortableMode::Manual {
        let index: &str = options
            .instance_hint
            .as_deref()
            .unwrap_or_default()
            .rsplit(':')
            .next()
            .unwrap_or_default();
        let instance: EmulatorInstance =
            instances
                .get(index)
                .cloned()
                .ok_or_else(|| PortableProbeError::Discovery {
                    message: format!(
                        "实例 {index} 不存在；候选为 {}",
                        describe_instances(instances)
                    ),
                })?;
        return Ok(instance);
    }

    if let Some(index) = &options.instance_hint
        && let Some(instance) = instances.get(index.rsplit(':').next().unwrap_or_default())
        && instance.is_process_started
    {
        return Ok(instance.clone());
    }

    if let Some(serial_hint) = &options.serial_hint {
        let matched: Vec<&EmulatorInstance> = instances
            .values()
            .filter(|instance: &&EmulatorInstance| {
                instance.is_process_started
                    && instance
                        .serial()
                        .is_ok_and(|serial: String| serial == *serial_hint)
            })
            .collect();
        if matched.len() == 1 {
            return Ok(matched[0].clone());
        }
    }

    let running: Vec<&EmulatorInstance> = instances
        .values()
        .filter(|instance: &&EmulatorInstance| instance.is_process_started)
        .collect();
    match running.len() {
        1 => Ok(running[0].clone()),
        0 => Err(PortableProbeError::Discovery {
            message: format!(
                "没有运行中的模拟器实例，请先启动目标实例；已登记实例为 {}",
                describe_instances(instances)
            ),
        }),
        _ => Err(PortableProbeError::AmbiguousTarget {
            message: format!(
                "发现多个正在运行的模拟器实例，请用 --instance 选择：{}",
                running
                    .iter()
                    .map(|instance: &&EmulatorInstance| describe_instance(instance))
                    .collect::<Vec<String>>()
                    .join("，")
            ),
        }),
    }
}

/// 只输出用户可理解的索引、名称和运行状态，不暴露无关内部字段。
#[cfg(target_os = "windows")]
fn describe_instances(instances: &BTreeMap<String, EmulatorInstance>) -> String {
    instances
        .values()
        .map(describe_instance)
        .collect::<Vec<String>>()
        .join("，")
}

/// 格式化单个实例的稳定用户提示，供完整集合和运行候选共用。
#[cfg(target_os = "windows")]
fn describe_instance(instance: &EmulatorInstance) -> String {
    let state: &str = if instance.is_ready() {
        "已启动"
    } else if instance.is_process_started {
        "启动中"
    } else {
        "已停止"
    };
    format!("{}:{} ({state})", instance.index, instance.name)
}
