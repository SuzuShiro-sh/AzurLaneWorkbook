/// 公共选择与状态测试使用的归一化样本，不依赖厂商响应格式。
#[cfg(test)]
pub(super) fn sample_instances()
-> std::collections::BTreeMap<String, suzushiro_emulator::EmulatorInstance> {
    [("0", true), ("1", false)]
        .into_iter()
        .map(|(index, started)| {
            (
                index.to_owned(),
                suzushiro_emulator::EmulatorInstance {
                    adb_host_ip: started.then(|| "127.0.0.1".to_owned()),
                    adb_port: started.then_some(16384),
                    android_version: Some("12.0".to_owned()),
                    available: true,
                    index: index.to_owned(),
                    is_android_started: started,
                    is_process_started: started,
                    name: format!("实例-{index}"),
                },
            )
        })
        .collect()
}
