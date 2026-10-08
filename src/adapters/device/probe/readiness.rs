//! 对运行态 RPC 就绪状态、完整快照与脱敏摘要执行有界校验。

use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use super::super::runtime::{
    AgentClient, AgentError, HealthResult, RetryDirective, RuntimeClientError, RuntimeShip,
    SessionEffect, SnapshotBagResult, SnapshotOwnedStateResult, SnapshotShipDetailsResult,
};
use super::{DEFAULT_TIMEOUT_MS, RuntimeProbeError, ShipGrowthSummary, UnsignedRangeSummary};

const QUEUE_READY_TIMEOUT: Duration = Duration::from_secs(15);
const SNAPSHOT_READY_TIMEOUT: Duration = Duration::from_secs(15);
const SNAPSHOT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// 保存认证连接达到完整业务可读状态时取得的有界等待证据。
pub(super) struct AgentReadiness {
    pub(super) health: HealthResult,
    pub(super) sample: FullStateReadySample,
}

/// 在固定期限内等待 agent 捕获游戏主线程并开放任务队列。
pub(super) fn wait_for_main_thread(
    client: &mut AgentClient,
) -> Result<HealthResult, RuntimeProbeError> {
    wait_for_main_thread_with(
        || {
            client
                .health(DEFAULT_TIMEOUT_MS)
                .map_err(RuntimeProbeError::from)
        },
        QUEUE_READY_TIMEOUT,
        Duration::from_millis(100),
    )
}

/// 对健康检查使用有界重试，使首次连接和后续持久读取遵循同一就绪语义。
pub(super) fn wait_for_main_thread_with(
    mut read_health: impl FnMut() -> Result<HealthResult, RuntimeProbeError>,
    readiness_timeout: Duration,
    retry_interval: Duration,
) -> Result<HealthResult, RuntimeProbeError> {
    let deadline: Instant = Instant::now() + readiness_timeout;
    loop {
        let health: HealthResult = read_health()?;
        if health.main_thread_queue_ready {
            return Ok(health);
        }
        if Instant::now() >= deadline {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "rpc.wait_main_thread",
                message: "15 秒内没有捕获到游戏主线程 Lua 状态".to_owned(),
            });
        }
        thread::sleep(retry_interval);
    }
}

/// 背包代理就绪样本。账号前后快照由正式读取在静态目录之后取得。
pub(super) struct FullStateReadySample {
    pub(super) bag: SnapshotBagResult,
    pub(super) bag_retries: u32,
}

/// 首次成功读取前执行完整背包探针，让登录未完成时尽早失败。
/// 同一会话已有成功读取后不再做这次探针。返回值只有重试次数，探针背包不进入随后的账号快照。
pub(super) fn bag_probe_retries_for_session_read<T>(
    prior_successful_reads: u64,
    mut probe: impl FnMut() -> Result<(T, u32), RuntimeProbeError>,
) -> Result<u32, RuntimeProbeError> {
    if prior_successful_reads > 0 {
        return Ok(0);
    }
    let (_snapshot, retries) = probe()?;
    Ok(retries)
}

/// 只等待背包代理就绪。账号状态和舰船详情留在正式采集位置重试。
pub(super) fn wait_for_full_state_readiness(
    client: &mut AgentClient,
    request_timeout_ms: u32,
    max_items: u32,
    _expected_module_sha256: &str,
) -> Result<FullStateReadySample, RuntimeProbeError> {
    let (bag, bag_retries): (SnapshotBagResult, u32) =
        wait_for_bag_snapshot(client, request_timeout_ms, max_items)?;
    Ok(FullStateReadySample { bag, bag_retries })
}

/// 等待游戏启动期 BagProxy 就绪；只接受 agent 明确声明可在同一会话重试的白名单错误。
pub(super) fn wait_for_bag_snapshot(
    client: &mut AgentClient,
    request_timeout_ms: u32,
    max_items: u32,
) -> Result<(SnapshotBagResult, u32), RuntimeProbeError> {
    wait_for_snapshot_readiness(
        client,
        request_timeout_ms,
        "rpc.wait_bag_proxy",
        "BagProxy",
        |client, bounded_timeout_ms| client.snapshot_bag(bounded_timeout_ms, max_items),
        is_retryable_bag_readiness_error,
    )
}

/// 统一执行主线程快照的有界轮询，仅接受调用方白名单内的同会话重试错误。
pub(super) fn wait_for_snapshot_readiness<T>(
    client: &mut AgentClient,
    request_timeout_ms: u32,
    stage: &'static str,
    subject: &str,
    mut request: impl FnMut(&mut AgentClient, u32) -> Result<T, RuntimeClientError>,
    is_retryable: fn(&AgentError) -> bool,
) -> Result<(T, u32), RuntimeProbeError> {
    poll_retryable_snapshot(
        request_timeout_ms,
        stage,
        subject,
        |timeout_ms| request(client, timeout_ms),
        is_retryable,
    )
}

/// 在调用方给定的单次读取上做有界白名单重试。正式账号采集与背包就绪共用这一规则。
pub(crate) fn poll_retryable_snapshot<T>(
    request_timeout_ms: u32,
    stage: &'static str,
    subject: &str,
    mut request: impl FnMut(u32) -> Result<T, RuntimeClientError>,
    is_retryable: fn(&AgentError) -> bool,
) -> Result<(T, u32), RuntimeProbeError> {
    let deadline: Instant = Instant::now() + SNAPSHOT_READY_TIMEOUT;
    let mut retry_count: u32 = 0;
    loop {
        let now: Instant = Instant::now();
        if now >= deadline {
            return Err(RuntimeProbeError::InvalidOutput {
                stage,
                message: format!("15 秒内 {subject} 未就绪，已重试 {retry_count} 次"),
            });
        }
        let remaining_ms: u128 = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(u128::from(request_timeout_ms))
            .max(1);
        let bounded_timeout_ms: u32 =
            u32::try_from(remaining_ms).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage,
                message: format!("{subject} 等待期限无法表示为 u32 毫秒"),
            })?;

        match request(bounded_timeout_ms) {
            Ok(snapshot) => return Ok((snapshot, retry_count)),
            Err(RuntimeClientError::Agent { error, .. }) if is_retryable(&error) => {
                retry_count =
                    retry_count
                        .checked_add(1)
                        .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                            stage,
                            message: format!("{subject} 重试计数溢出"),
                        })?;
                if Instant::now() >= deadline {
                    return Err(RuntimeProbeError::InvalidOutput {
                        stage,
                        message: format!(
                            "15 秒内 {subject} 未就绪，最后错误为 {}，共重试 {retry_count} 次",
                            error.code
                        ),
                    });
                }
                thread::sleep(
                    SNAPSHOT_RETRY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// 只允许错误码、重试指令和会话影响同时满足白名单时继续轮询。
pub(super) fn is_retryable_bag_readiness_error(error: &AgentError) -> bool {
    const RETRYABLE_CODES: [&str; 5] = [
        "lua_get_proxy_missing",
        "lua_get_proxy_failed",
        "lua_bag_proxy_invalid",
        "lua_bag_data_missing",
        "lua_bag_data_invalid",
    ];

    error.retry == RetryDirective::SameRequest
        && error.session_effect == SessionEffect::Unchanged
        && RETRYABLE_CODES.contains(&error.code.as_str())
}

/// 完整运行态轮询只扩展到各只读代理明确声明的初始化中状态。
pub(crate) fn is_retryable_owned_state_readiness_error(error: &AgentError) -> bool {
    const RETRYABLE_CODES: [&str; 12] = [
        "lua_get_proxy_missing",
        "lua_get_proxy_failed",
        "lua_bay_proxy_invalid",
        "lua_bay_data_invalid",
        "lua_equipment_proxy_invalid",
        "lua_equipment_data_invalid",
        "lua_equipments_invalid",
        "lua_player_proxy_invalid",
        "lua_player_data_invalid",
        "lua_bag_proxy_invalid",
        "lua_bag_data_missing",
        "lua_bag_data_invalid",
    ];

    error.retry == RetryDirective::SameRequest
        && error.session_effect == SessionEffect::Unchanged
        && RETRYABLE_CODES.contains(&error.code.as_str())
}

/// 舰船详情只对白名单内的 Lua 与船坞初始化中状态执行同会话轮询。
pub(crate) fn is_retryable_ship_details_readiness_error(error: &AgentError) -> bool {
    const RETRYABLE_CODES: [&str; 5] = [
        "lua_api_not_ready",
        "lua_get_proxy_missing",
        "lua_get_proxy_failed",
        "lua_bay_proxy_invalid",
        "lua_bay_data_invalid",
    ];

    error.retry == RetryDirective::SameRequest
        && error.session_effect == SessionEffect::Unchanged
        && RETRYABLE_CODES.contains(&error.code.as_str())
}

/// 拒绝会被业务层误用的部分运行态，并保留有界、去标识化的字段错误原因。
pub(crate) fn validate_complete_owned_state(
    snapshot: &SnapshotOwnedStateResult,
) -> Result<(), RuntimeProbeError> {
    if snapshot.complete {
        return Ok(());
    }

    let dock_errors = summarize_read_errors(
        snapshot.dock.read_errors.len(),
        snapshot
            .dock
            .read_errors
            .iter()
            .map(|error| (error.code.as_str(), error.message.as_str())),
    );
    let warehouse_errors = summarize_read_errors(
        snapshot.warehouse.read_errors.len(),
        snapshot
            .warehouse
            .read_errors
            .iter()
            .map(|error| (error.code.as_str(), error.message.as_str())),
    );
    let bag_errors = summarize_read_errors(
        snapshot.bag.read_errors.len(),
        snapshot
            .bag
            .read_errors
            .iter()
            .map(|error| (error.code.as_str(), error.message.as_str())),
    );
    Err(RuntimeProbeError::InvalidOutput {
        stage: "rpc.snapshot_owned_state",
        message: format!(
            "完整运行态不完整: dock(complete={}, truncated={}, count={}, errors={dock_errors}); \
             warehouse(complete={}, truncated={}, count={}, errors={warehouse_errors}); \
             bag(complete={}, truncated={}, count={}, errors={bag_errors})",
            snapshot.dock.complete,
            snapshot.dock.truncated,
            snapshot.dock.count,
            snapshot.warehouse.complete,
            snapshot.warehouse.truncated,
            snapshot.warehouse.count,
            snapshot.bag.complete,
            snapshot.bag.truncated,
            snapshot.bag.count,
        ),
    })
}

/// 拒绝截断或逐项失败的详情，错误摘要不包含舰船和技能标识。
pub(crate) fn validate_complete_ship_details(
    snapshot: &SnapshotShipDetailsResult,
) -> Result<(), RuntimeProbeError> {
    if snapshot.complete {
        return Ok(());
    }

    let errors = summarize_read_errors(
        snapshot.read_errors.len(),
        snapshot
            .read_errors
            .iter()
            .map(|error| (error.code.as_str(), error.message.as_str())),
    );
    Err(RuntimeProbeError::InvalidOutput {
        stage: "rpc.snapshot_ship_details",
        message: format!(
            "舰船详情不完整: complete={}, truncated={}, count={}, errors={errors}",
            snapshot.complete, snapshot.truncated, snapshot.count
        ),
    })
}

/// 生成不包含任何运行态标识的养成摘要，并显式拒绝计数溢出。
pub(super) fn summarize_ship_growth(
    ships: &[RuntimeShip],
) -> Result<ShipGrowthSummary, RuntimeProbeError> {
    let mut config_ids: HashSet<u64> = HashSet::with_capacity(ships.len());
    let mut skill_count: u64 = 0;
    let mut ships_without_skills: u32 = 0;
    let mut level: Option<UnsignedRangeSummary> = None;
    let mut experience_in_level: Option<UnsignedRangeSummary> = None;
    let mut intimacy_raw: Option<UnsignedRangeSummary> = None;
    let mut energy: Option<UnsignedRangeSummary> = None;
    let mut proficiency: Option<UnsignedRangeSummary> = None;
    let mut skill_level: Option<UnsignedRangeSummary> = None;
    let mut skill_experience: Option<UnsignedRangeSummary> = None;

    for ship in ships {
        config_ids.insert(ship.config_id);
        UnsignedRangeSummary::include(&mut level, u64::from(ship.level));
        UnsignedRangeSummary::include(&mut experience_in_level, ship.experience_in_level);
        UnsignedRangeSummary::include(&mut intimacy_raw, ship.intimacy_raw);
        UnsignedRangeSummary::include(&mut energy, ship.energy);
        UnsignedRangeSummary::include(&mut proficiency, ship.proficiency);
        if ship.skills.is_empty() {
            ships_without_skills = ships_without_skills.checked_add(1).ok_or_else(|| {
                RuntimeProbeError::InvalidOutput {
                    stage: "rpc.ship_growth_summary",
                    message: "无自身技能舰船计数溢出 u32".to_owned(),
                }
            })?;
        }
        skill_count = skill_count
            .checked_add(ship.skills.len() as u64)
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "rpc.ship_growth_summary",
                message: "自身技能总数溢出 u64".to_owned(),
            })?;
        for skill in &ship.skills {
            UnsignedRangeSummary::include(&mut skill_level, u64::from(skill.level));
            UnsignedRangeSummary::include(&mut skill_experience, skill.experience);
        }
    }

    let distinct_config_count: u32 =
        u32::try_from(config_ids.len()).map_err(|_| RuntimeProbeError::InvalidOutput {
            stage: "rpc.ship_growth_summary",
            message: "不同舰船配置数量超出 u32".to_owned(),
        })?;
    Ok(ShipGrowthSummary {
        distinct_config_count,
        skill_count,
        ships_without_skills,
        level,
        experience_in_level,
        intimacy_raw,
        energy,
        proficiency,
        skill_level,
        skill_experience,
    })
}

/// 最多保留前三个稳定错误及原因，其余只记录数量以限制日志体积。
pub(super) fn summarize_read_errors<'a>(
    error_count: usize,
    errors: impl Iterator<Item = (&'a str, &'a str)>,
) -> String {
    let mut summaries: Vec<String> = errors
        .take(3)
        .map(|(code, message)| format!("{code}: {message}"))
        .collect();
    if error_count > summaries.len() {
        summaries.push(format!("其余 {} 项", error_count - summaries.len()));
    }
    if summaries.is_empty() {
        "无".to_owned()
    } else {
        summaries.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::bag_probe_retries_for_session_read;
    use crate::adapters::device::probe::RuntimeProbeError;

    #[test]
    fn later_session_reads_skip_the_full_bag_probe_and_keep_its_snapshot_out_of_the_result() {
        let mut probes = 0_u32;
        let first = bag_probe_retries_for_session_read(0, || {
            probes += 1;
            Ok(("full-bag", 2_u32))
        })
        .unwrap();
        assert_eq!(probes, 1);
        assert_eq!(first, 2);

        let later = bag_probe_retries_for_session_read(1, || {
            probes += 1;
            Ok(("full-bag", 9_u32))
        })
        .unwrap();
        assert_eq!(probes, 1, "已有成功读取后不得再取完整背包");
        assert_eq!(later, 0);

        let error = bag_probe_retries_for_session_read(0, || {
            probes += 1;
            Err::<(&str, u32), _>(RuntimeProbeError::InvalidOutput {
                stage: "rpc.wait_bag_proxy",
                message: "背包未就绪".to_owned(),
            })
        })
        .unwrap_err();
        assert_eq!(probes, 2);
        assert!(matches!(
            error,
            RuntimeProbeError::InvalidOutput {
                stage: "rpc.wait_bag_proxy",
                ..
            }
        ));
    }
}
