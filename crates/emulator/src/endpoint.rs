//! 根据 Windows TCP 监听者绑定回环端点，避免不同模拟器共用端口时连错设备。

use super::EmulatorError;
use std::net::Ipv4Addr;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;

#[derive(Clone, Copy, Debug)]
pub(super) struct TcpListenerOwner {
    address: Ipv4Addr,
    port: u16,
    pid: u32,
}

pub(super) fn list_tcp_listeners() -> Result<Vec<TcpListenerOwner>, EmulatorError> {
    let failure = |message: String| EmulatorError::Discovery { message };
    let mut size = 0u32;
    let status = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };
    if status != 122 && status != 0 {
        return Err(failure(format!("读取监听表容量失败: {status}")));
    }
    // 两次调用间系统监听表可能变化；只在系统明确报告容量不足时重取。
    for _ in 0..3 {
        if !(4..=4 * 1024 * 1024).contains(&size) {
            return Err(failure(format!("监听表容量无效: {size}")));
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let capacity = buffer.len() * 8;
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                AF_INET as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == 122 {
            continue;
        }
        if status != 0 {
            return Err(failure(format!("读取监听表失败: {status}")));
        }
        let bytes = buffer.as_ptr().cast::<u8>();
        let count = unsafe { std::ptr::read_unaligned(bytes.cast::<u32>()) } as usize;
        let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        if count > (capacity - 4) / row_size {
            return Err(failure("监听表记录数量超出缓冲区".to_owned()));
        }
        let mut listeners = Vec::with_capacity(count);
        for index in 0..count {
            let row = unsafe {
                std::ptr::read_unaligned(
                    bytes
                        .add(4 + index * row_size)
                        .cast::<MIB_TCPROW_OWNER_PID>(),
                )
            };
            listeners.push(TcpListenerOwner {
                address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()),
                port: u16::from_be(row.dwLocalPort as u16),
                pid: row.dwOwningPid,
            });
        }
        return Ok(listeners);
    }
    Err(failure("监听表持续变化，请刷新实例目录".to_owned()))
}

/// 精确地址优先于通配地址；只选择唯一且属于当前提供方的实际监听者。
pub(super) fn select_owned_loopback(
    listeners: &[TcpListenerOwner],
    port: u16,
    owns: impl Fn(u32) -> bool,
) -> Option<Ipv4Addr> {
    for address in [Ipv4Addr::new(127, 0, 0, 1), Ipv4Addr::new(127, 0, 0, 2)] {
        let exact: Vec<_> = listeners
            .iter()
            .filter(|listener| listener.port == port && listener.address == address)
            .collect();
        let matches = if exact.is_empty() {
            listeners
                .iter()
                .filter(|listener| listener.port == port && listener.address.is_unspecified())
                .collect()
        } else {
            exact
        };
        if matches.len() == 1 && owns(matches[0].pid) {
            return Some(address);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skips_other_emulator_on_specific_loopback_address() {
        let listeners = [
            TcpListenerOwner {
                address: Ipv4Addr::LOCALHOST,
                port: 5555,
                pid: 10,
            },
            TcpListenerOwner {
                address: Ipv4Addr::UNSPECIFIED,
                port: 5555,
                pid: 20,
            },
        ];
        assert_eq!(
            select_owned_loopback(&listeners, 5555, |pid| pid == 20),
            Some(Ipv4Addr::new(127, 0, 0, 2))
        );
        assert_eq!(
            select_owned_loopback(&listeners, 5555, |pid| pid == 30),
            None
        );
    }
    #[test]
    fn ambiguous_owner_is_not_usable() {
        let listeners = [TcpListenerOwner {
            address: Ipv4Addr::UNSPECIFIED,
            port: 5555,
            pid: 20,
        }; 2];
        assert_eq!(select_owned_loopback(&listeners, 5555, |_| true), None);
    }
}
