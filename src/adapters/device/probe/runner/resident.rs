//! 保存受 Windows 用户身份保护的驻留凭据，并在排他所有权下重新认证。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{Ordering, compiler_fence};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

use super::super::cleanup::CleanupEvidence;
use super::super::device_bridge::{PROCESS_PROBE_COMMAND_TIMEOUT, parse_boot_id};
use super::*;
use crate::adapters::tool_root::ToolRootError;

const MAXIMUM_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResidentRecord {
    schema: u32,
    serial: String,
    boot_id: String,
    session_id: SessionId,
    secret: SessionSecret,
    channel_id: SessionId,
    mapping_id: SessionId,
    package: String,
    loader_sha256: String,
    agent_sha256: String,
    profile_sha256: String,
    profile_json: Vec<u8>,
    module_sha256: String,
    launch_component: Option<String>,
    baseline: ProcessEvidence,
    loader: Option<LoaderReceipt>,
}

/// 文件句柄持有期间禁止其他宿主接管同一管理器实例和包。
pub(super) struct ResidentStore {
    root: ToolRoot,
    relative: PathBuf,
    _lock: File,
}

fn failure(message: impl Into<String>) -> RuntimeProbeError {
    RuntimeProbeError::InvalidOutput {
        stage: "resident.session",
        message: message.into(),
    }
}

/// 解密和序列化缓冲区在所有退出路径上清零，不进入错误正文。
struct SensitiveBytes(Vec<u8>);
impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // 缓冲区由本对象独占，易失写入阻止优化器省略清零。
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

fn protect(bytes: &[u8], decrypt: bool) -> io::Result<SensitiveBytes> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(io::Error::other)?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // DPAPI 绑定当前 Windows 用户；禁止任何交互提示，输出由 LocalFree 释放。
    let success = unsafe {
        if decrypt {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    // DPAPI 成功返回有效分配；复制后先清空原分配再释放。
    let result = unsafe {
        let raw = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let result = SensitiveBytes(raw.to_vec());
        for byte in raw {
            std::ptr::write_volatile(byte, 0);
        }
        compiler_fence(Ordering::SeqCst);
        LocalFree(output.pbData.cast());
        result
    };
    Ok(result)
}

impl ResidentStore {
    pub(super) fn acquire(
        root: &ToolRoot,
        options: &RuntimeProbeOptions,
        package: &str,
    ) -> Result<Self, RuntimeProbeError> {
        let manager = std::fs::canonicalize(&options.manager_executable).map_err(|source| {
            RuntimeProbeError::Io {
                stage: "resident.manager_identity",
                path: options.manager_executable.clone().into(),
                source,
            }
        })?;
        let key = sha256_json(
            &(
                manager.to_string_lossy().to_lowercase(),
                &options.vm_index,
                package,
            ),
            "resident.target_key",
        )?;
        root.ensure_directory(Path::new("data/resident"))?;
        let relative = Path::new("data/resident").join(&key);
        let lock_relative = relative.with_extension("lock");
        let path = root
            .prepare_new_file(&lock_relative)
            .or_else(|error| match error {
                ToolRootError::PathConflict { .. } => root.existing_file(&lock_relative),
                other => Err(other),
            })?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
            .map_err(|source| RuntimeProbeError::Io {
                stage: "resident.lock",
                path: path.clone(),
                source,
            })?;
        root.ensure_open_file_matches(&lock, &path)?;
        Ok(Self {
            root: root.clone(),
            relative,
            _lock: lock,
        })
    }

    fn read(&self) -> Result<Option<ResidentRecord>, RuntimeProbeError> {
        for extension in ["session", "pending"] {
            let relative = self.relative.with_extension(extension);
            if !self
                .root
                .as_path()
                .join(&relative)
                .try_exists()
                .map_err(|e| failure(e.to_string()))?
            {
                continue;
            }
            let snapshot = read_bounded_file_snapshot(&self.root, &relative, MAXIMUM_RECORD_BYTES)
                .map_err(|e| failure(format!("驻留记录读取失败: {e}")))?;
            let (bytes, _) = snapshot.into_parts();
            let plain =
                protect(&bytes, true).map_err(|e| failure(format!("驻留记录解密失败: {e}")))?;
            // 不公开反序列化错误正文，避免被破坏的凭据进入日志。
            return serde_json::from_slice(&plain.0)
                .map(Some)
                .map_err(|_| failure("驻留记录格式无效，已保留原记录"));
        }
        Ok(None)
    }

    fn save(&self, record: &ResidentRecord) -> Result<(), RuntimeProbeError> {
        let plain =
            SensitiveBytes(serde_json::to_vec(record).map_err(|_| failure("驻留记录编码失败"))?);
        let encrypted =
            protect(&plain.0, false).map_err(|e| failure(format!("驻留记录加密失败: {e}")))?;
        let relative = self.relative.with_extension(if record.loader.is_some() {
            "session"
        } else {
            "pending"
        });
        let temporary = self
            .relative
            .with_extension(format!("{}.tmp", SessionId::generate()?));
        suzushiro_artifact_publish::publish_new_with(
            &self.root,
            &temporary,
            &relative,
            MAXIMUM_RECORD_BYTES,
            |writer| writer.write_all(&encrypted.0),
        )
        .map_err(|e| failure(format!("驻留记录发布失败: {e:?}")))?;
        if record.loader.is_some() {
            self.root
                .remove_file_if_exists(&self.relative.with_extension("pending"), None)?;
        }
        Ok(())
    }

    fn remove(&self) -> Result<(), RuntimeProbeError> {
        self.root
            .remove_file_if_exists(&self.relative.with_extension("session"), None)?;
        self.root
            .remove_file_if_exists(&self.relative.with_extension("pending"), None)?;
        Ok(())
    }
}

impl ProductionSession {
    pub(in crate::adapters::device::probe) fn remove_resident_record(
        &mut self,
    ) -> Result<(), RuntimeProbeError> {
        if self.resident_record_owned {
            self.resident_store.remove()?;
            self.resident_record_owned = false;
        }
        Ok(())
    }

    fn boot_id(&self) -> Result<String, RuntimeProbeError> {
        parse_boot_id(
            &self
                .bridge
                .root_checked("cat /proc/sys/kernel/random/boot_id", "resident.boot_id")?,
            "resident.boot_id",
        )
    }

    pub(super) fn save_resident(
        &mut self,
        baseline: &ProcessEvidence,
        loader: Option<&LoaderReceipt>,
    ) -> Result<(), RuntimeProbeError> {
        let (profile_json, profile_sha256) =
            read_runtime_input_snapshot(&self.tool_root, PROFILE_RELATIVE_PATH, MAX_PROFILE_BYTES)?;
        if profile_sha256 != self.profile_sha256 {
            return Err(failure("加载期间 profile 文件发生变化"));
        }
        let record = ResidentRecord {
            schema: 1,
            serial: self.options.serial.clone(),
            boot_id: self.boot_id()?,
            session_id: self.session_id,
            secret: self.session_secret.clone(),
            channel_id: self.channel_id,
            mapping_id: self.mapping_id,
            package: self.profile.bootstrap().package_name().to_owned(),
            loader_sha256: self.loader_sha256.clone(),
            agent_sha256: self.agent_sha256.clone(),
            profile_sha256: self.profile_sha256.clone(),
            profile_json,
            module_sha256: self.profile.bootstrap().module_sha256().to_owned(),
            launch_component: self.launch_component.clone(),
            baseline: baseline.clone(),
            loader: loader.cloned(),
        };
        self.resident_store.save(&record)?;
        self.resident_record_owned = true;
        Ok(())
    }

    /// 只在原进程确实消失后移除旧记录；版本或认证失败均保留现场。
    /// 一次性审计不能接管仍在运行的驻留代理。现场保留，调用方负责停止。
    pub(super) fn reject_live_resident(&mut self) -> Result<(), RuntimeProbeError> {
        if self.live_resident_record()?.is_some() {
            Err(failure("已有驻留代理，请先显式卸载再执行一次性审计"))
        } else {
            Ok(())
        }
    }

    fn live_resident_record(&mut self) -> Result<Option<ResidentRecord>, RuntimeProbeError> {
        // 读取失败也必须禁止后续析构对原驻留状态执行卸载或删除。
        self.resident_preserved = true;
        let Some(record) = self.resident_store.read()? else {
            self.resident_preserved = false;
            return Ok(None);
        };
        if record.schema != 1 {
            return Err(failure("驻留记录版本不受支持"));
        }
        if self.boot_id()? != record.boot_id
            || self.bridge.process_instance_absent(
                record.baseline.process_id,
                Some(record.baseline.process_start_time),
                PROCESS_PROBE_COMMAND_TIMEOUT,
            )?
        {
            self.tool_root.remove_directory_if_exists(
                &Path::new("data/temp").join(record.session_id.to_string()),
            )?;
            self.resident_store.remove()?;
            self.resident_preserved = false;
            return Ok(None);
        }
        Ok(Some(record))
    }

    pub(super) fn reconnect_resident(
        &mut self,
        unloading: bool,
    ) -> Result<Option<AuthenticatedAgent>, RuntimeProbeError> {
        let Some(record) = self.live_resident_record()? else {
            return Ok(None);
        };
        if record.serial != self.options.serial
            || record.package != self.profile.bootstrap().package_name()
            || (!unloading
                && (record.loader_sha256 != self.loader_sha256
                    || record.agent_sha256 != self.agent_sha256
                    || record.profile_sha256 != self.profile_sha256))
        {
            return Err(failure(
                "驻留代理目标或资源版本与当前安装不一致，请使用原安装卸载或结束原游戏进程",
            ));
        }
        if suzushiro_content_digest::sha256_bytes(&record.profile_json) != record.profile_sha256 {
            return Err(failure("驻留 profile 内容与原摘要不一致"));
        }
        if unloading {
            self.profile = RuntimeProfile::from_slice(&record.profile_json)?;
            if self.profile.bootstrap().package_name() != record.package {
                return Err(failure("驻留 profile 目标包不一致"));
            }
            self.loader_sha256 = record.loader_sha256.clone();
            self.agent_sha256 = record.agent_sha256.clone();
            self.profile_sha256 = record.profile_sha256.clone();
        }
        let loader = record.loader.ok_or_else(|| {
            failure("上次代理加载没有完整收据，已保留诊断记录；请结束原游戏进程后重试")
        })?;
        if loader.process_id != record.baseline.process_id
            || loader.process_start_time != record.baseline.process_start_time
        {
            return Err(failure("驻留加载收据与原进程身份不一致"));
        }
        loader.validate_loaded(record.baseline.process_id)?;
        if !unloading
            && (loader.agent_mapping_mode != self.options.agent_mapping_mode
                || loader.agent_visibility_mode != self.options.agent_visibility_mode)
        {
            return Err(failure("驻留代理映射策略与当前设置不一致，请先显式卸载"));
        }
        let module = self.bridge.read_module_sha256(
            record.baseline.process_id,
            self.profile.bootstrap().module_name(),
        )?;
        if module != record.module_sha256 {
            return Err(failure("驻留代理对应的游戏模块发生变化"));
        }
        self.profile.bind_module_sha256(module.clone())?;
        let process = self
            .bridge
            .collect_process_evidence(record.baseline.process_id)?;
        validate_preserved_process_identity(
            &process,
            record.baseline.process_id,
            Some(record.baseline.process_start_time),
            "resident.process_identity",
        )?;
        self.tool_root
            .remove_directory_if_exists(&self.host_session_relative)?;
        self.session_id = record.session_id;
        self.session_secret = record.secret;
        self.channel_id = record.channel_id;
        self.mapping_id = record.mapping_id;
        self.target_pid = record.baseline.process_id;
        self.expected_process_start_time = Some(record.baseline.process_start_time);
        self.launch_component = record.launch_component;
        self.remote_endpoint = format!("localabstract:{}", self.channel_id);
        self.device_session_dir = format!("/data/local/tmp/.{}", self.session_id);
        self.device_loader_path = format!("{}/0", self.device_session_dir);
        self.device_agent_path = format!("{}/1", self.device_session_dir);
        self.device_bootstrap_path = format!("{}/2", self.device_session_dir);
        self.stage_loader_path = format!("{}.0", self.device_session_dir);
        self.stage_agent_path = format!("{}.1", self.device_session_dir);
        self.stage_bootstrap_path = format!("{}.2", self.device_session_dir);
        self.stage_unload_path = format!("{}.3", self.device_session_dir);
        self.host_session_relative = Path::new("data/temp").join(self.session_id.to_string());
        self.host_session_dir = self
            .tool_root
            .ensure_directory(&self.host_session_relative)?;
        self.host_bootstrap_relative = self.host_session_relative.join("bootstrap.bin");
        self.host_bootstrap_path = self.host_session_dir.join("bootstrap.bin");
        self.resident_record_owned = true;
        if unloading {
            self.verify_remote_asset_hashes()?;
        }
        let forward_port = self.create_forward()?;
        let expected = ExpectedAgent::new(
            self.session_id,
            self.target_pid,
            self.profile.bootstrap().package_name(),
            self.profile.abi(),
        )?;
        let client = AgentClient::connect(
            SocketAddr::new(IpAddr::from([127, 0, 0, 1]), forward_port),
            expected,
            self.session_secret.clone(),
            self.options.connect_timeout,
        )?;
        self.journal.record("session.reconnected", "ok", json!({
            "host_session_id": self.host_session_id, "session_id": self.session_id, "agent_session_id": self.session_id,
            "process_id": self.target_pid, "process_start_time": record.baseline.process_start_time,
            "loader_receipt": loader, "before_load": record.baseline, "process": process,
            "loader_sha256": self.loader_sha256, "agent_sha256": self.agent_sha256,
            "profile_sha256": self.profile_sha256, "module_sha256": module,
        }))?;
        Ok(Some(AuthenticatedAgent {
            handshake_attempts: client.handshake_attempts(),
            client,
            loader_receipt: loader,
            forward_port,
            oversized_frame_closed_connection: false,
            before_load: record.baseline,
        }))
    }

    /// 仅回收本宿主创建的 forward；设备代理、载入收据和凭据继续由驻留记录持有。
    pub(in crate::adapters::device::probe) fn detach_resident(
        &mut self,
    ) -> Result<CleanupResult, RuntimeProbeError> {
        if let Some(result) = &self.cleanup_result {
            return Ok(result.clone());
        }
        if self.forward_creation_attempted {
            let forwards = self.bridge.forward_list()?;
            if let Some(port) = find_owned_forward_port(&forwards, &self.remote_endpoint)? {
                self.bridge.adb_checked(
                    &[
                        "forward".to_owned(),
                        "--remove".to_owned(),
                        format!("tcp:{port}"),
                    ],
                    "resident.disconnect",
                )?;
            }
            self.forward_port = None;
            self.forward_creation_attempted = false;
        }
        let resident_record_verified = if self.resident_record_owned {
            let record = self
                .resident_store
                .read()?
                .ok_or_else(|| failure("驻留记录丢失，不能确认恢复身份"))?;
            if record.session_id != self.session_id
                || record.channel_id != self.channel_id
                || record.mapping_id != self.mapping_id
                || record.baseline.process_id != self.target_pid
                || Some(record.baseline.process_start_time) != self.expected_process_start_time
            {
                return Err(failure("驻留记录读回身份不一致"));
            }
            if record.loader.is_none() {
                return Err(failure("驻留记录尚无完整加载收据，已保留诊断现场"));
            }
            true
        } else {
            false
        };
        let evidence = if self.target_pid > 0 {
            self.bridge.collect_process_evidence(self.target_pid)?
        } else {
            super::super::process_evidence::empty_process_evidence(0)
        };
        if resident_record_verified {
            validate_preserved_process_identity(
                &evidence,
                self.target_pid,
                self.expected_process_start_time,
                "resident.detach_identity",
            )?;
        }
        let forward_list_restored = match &self.forwards_before {
            Some(before) => *before == self.bridge.forward_list()?,
            None => true,
        };
        if !forward_list_restored {
            return Err(failure("断开代理后 forward 列表未恢复"));
        }
        let host_session_removed = if !self.resident_record_owned {
            self.tool_root
                .remove_directory_if_exists(&self.host_session_relative)?;
            true
        } else {
            false
        };
        let result = CleanupResult {
            evidence,
            cleanup: CleanupEvidence {
                forward_removed: true,
                forward_list_restored,
                device_session_removed: false,
                host_session_removed,
                old_process_stopped: false,
                game_restarted: false,
            },
        };
        self.journal.record("session.detached", "ok", json!({
            "host_session_id": self.host_session_id, "session_id": self.session_id, "process_id": self.target_pid,
            "process_start_time": result.evidence.process_start_time, "process": result.evidence,
            "forward_removed": result.cleanup.forward_removed, "forward_list_restored": result.cleanup.forward_list_restored,
            "resident_record_verified": resident_record_verified,
        }))?;
        self.cleanup_result = Some(result.clone());
        Ok(result)
    }
}

impl ProductionSession {
    pub(in crate::adapters::device::probe) fn manage_resident(
        options: RuntimeProbeOptions,
        action: super::super::super::game_port::AgentAction,
    ) -> Result<Option<crate::adapters::device::runtime::HealthResult>, RuntimeProbeError> {
        use super::super::super::game_port::AgentAction;
        let mut runner = Self::new(options, None)?;
        runner.forwards_before = Some(runner.bridge.forward_list()?);
        runner.bridge.verify_target()?;
        let connection = match action {
            AgentAction::Inject => Some(runner.open_authenticated_agent()?),
            _ => runner.reconnect_resident(action == AgentAction::Unload)?,
        };
        let Some(mut connection) = connection else {
            runner.resident_preserved = true;
            runner.detach_resident()?;
            return Ok(None);
        };
        let health = connection.client.health(runner.options.timeout_ms)?;
        if action == AgentAction::Unload {
            runner.resident_preserved = false;
            let mut journal_errors = Vec::new();
            let operation = runner.unload_agent_gracefully(connection, &mut journal_errors);
            let journal_path = runner.journal.path.clone();
            if let Err(source) = operation {
                return Err(probe_failed_after_cleanup(source, journal_path, || {
                    runner.cleanup()
                }));
            }
            runner.cleanup()?;
            if !journal_errors.is_empty() {
                return Err(failure(journal_errors.join("；")));
            }
        } else {
            drop(connection);
            runner.detach_resident()?;
        }
        Ok(Some(health))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            let home = std::env::var_os("USERPROFILE").expect("测试需要用户目录");
            let path = PathBuf::from(home)
                .join("suzushiro/scratch/azlw-resident-tests")
                .join(SessionId::generate().unwrap().to_string());
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn options(&self) -> RuntimeProbeOptions {
            RuntimeProbeOptions::new(
                &self.0,
                std::env::current_exe().unwrap().to_str().unwrap(),
                "adb.exe",
                "0",
                "127.0.0.1:16384",
            )
            .unwrap()
        }
        fn store(&self) -> ResidentStore {
            ResidentStore::acquire(
                &ToolRoot::open(&self.0).unwrap(),
                &self.options(),
                "com.example.game",
            )
            .unwrap()
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn record() -> ResidentRecord {
        let mut baseline = super::super::super::process_evidence::empty_process_evidence(42);
        baseline.process_start_time = 99;
        ResidentRecord {
            schema: 1,
            serial: "127.0.0.1:16384".to_owned(),
            boot_id: "00112233-4455-6677-8899-aabbccddeeff".to_owned(),
            session_id: SessionId::generate().unwrap(),
            secret: SessionSecret::generate().unwrap(),
            channel_id: SessionId::generate().unwrap(),
            mapping_id: SessionId::generate().unwrap(),
            package: "com.example.game".to_owned(),
            loader_sha256: "1".repeat(64),
            agent_sha256: "2".repeat(64),
            profile_sha256: "3".repeat(64),
            profile_json: b"{}".to_vec(),
            module_sha256: "4".repeat(64),
            launch_component: None,
            baseline,
            loader: None,
        }
    }

    #[test]
    fn dpapi_round_trip_and_corruption_rejection() {
        let plain = b"resident-session-secret";
        let encrypted = protect(plain, false).unwrap();
        assert_ne!(encrypted.0, plain);
        assert_eq!(protect(&encrypted.0, true).unwrap().0, plain);
        let mut damaged = encrypted.0.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        assert!(protect(&damaged, true).is_err());
        assert!(protect(b"not-dpapi", true).is_err());
    }

    #[test]
    fn target_lock_is_exclusive_and_released_by_owner_drop() {
        let root = TestRoot::new();
        let first = root.store();
        let error = ResidentStore::acquire(&first.root, &root.options(), "com.example.game")
            .err()
            .unwrap();
        assert!(matches!(
            error,
            RuntimeProbeError::Io {
                stage: "resident.lock",
                ..
            }
        ));
        // 不同游戏包拥有独立锁，不与本目标相互阻塞。
        let other =
            ResidentStore::acquire(&first.root, &root.options(), "com.example.other").unwrap();
        drop(other);
        drop(first);
        drop(root.store());
    }

    #[test]
    fn encrypted_record_survives_owner_restart_and_does_not_overwrite_pending() {
        let root = TestRoot::new();
        let store = root.store();
        assert!(store.read().unwrap().is_none());
        let record = record();
        store.save(&record).unwrap();
        let encoded_secret = serde_json::to_vec(&record.secret).unwrap();
        let on_disk = std::fs::read(
            store
                .root
                .as_path()
                .join(store.relative.with_extension("pending")),
        )
        .unwrap();
        assert!(
            !on_disk
                .windows(encoded_secret.len())
                .any(|bytes| bytes == encoded_secret)
        );
        assert!(store.save(&record).is_err());
        drop(store);
        let reopened = root.store();
        let restored = reopened.read().unwrap().unwrap();
        assert_eq!(restored.session_id, record.session_id);
        assert_eq!(restored.baseline, record.baseline);
        assert_eq!(
            serde_json::to_vec(&restored.secret).unwrap(),
            encoded_secret
        );
        reopened.remove().unwrap();
        assert!(reopened.read().unwrap().is_none());
    }

    #[test]
    fn complete_receipt_supersedes_pending_and_corruption_keeps_evidence() {
        let root = TestRoot::new();
        let store = root.store();
        let mut record = record();
        store.save(&record).unwrap();
        record.loader = Some(serde_json::from_slice(&serde_json::to_vec(&json!({
            "status":"ok","code":"loaded","message":"ok","process_id":42,"process_start_time":99,
            "agent_handle":"0000000000007000","agent_base":"0000000000100000","agent_load_size":131072,
            "finalize_address":"0000000000101000","agent_mapping_name":"/memfd:fixture (deleted)",
            "agent_mapping_mode":"memfd","agent_visibility_mode":"normal","agent_soinfo_address":null,
            "agent_protected_elf_header":null,"anonymous_segment_count":0,"anonymous_byte_count":0
        })).unwrap()).unwrap());
        store.save(&record).unwrap();
        assert!(
            !store
                .root
                .as_path()
                .join(store.relative.with_extension("pending"))
                .exists()
        );
        assert_eq!(store.read().unwrap().unwrap().loader, record.loader);
        let path = store
            .root
            .as_path()
            .join(store.relative.with_extension("session"));
        std::fs::write(&path, b"damaged").unwrap();
        assert!(store.read().is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"damaged");
    }
}
