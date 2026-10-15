use std::net::{TcpStream, ToSocketAddrs};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::model::ServerEntry;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 不创建控制台窗口。
///
/// 本程序是 GUI 子系统、自身没有控制台；此时启动控制台程序（如 cmdkey.exe），
/// Windows 会为子进程分配一个新控制台并闪出黑窗口。加上该标志可避免。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 测量到服务器 RDP 端口的 TCP 连接延迟（毫秒），失败返回 None
pub fn check_latency(host: &str, port: u16) -> Option<u128> {
    let addr = format!("{host}:{port}").to_socket_addrs().ok()?.next()?;
    let start = Instant::now();
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    Some(start.elapsed().as_millis())
}

/// 并发检测所有服务器延迟，返回 (id, 延迟) 列表
pub fn check_all(servers: &[ServerEntry]) -> Vec<(String, Option<u128>)> {
    std::thread::scope(|scope| {
        servers
            .iter()
            .map(|s| scope.spawn(|| (s.id.clone(), check_latency(&s.host, s.port))))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap_or((String::new(), None)))
            .collect()
    })
}

/// 通过 mstsc 连接服务器：写入凭据 -> 启动 mstsc -> 窗口关闭后删除凭据。
/// 整个流程在独立线程中执行，不阻塞界面。
pub fn connect(server: ServerEntry, password: String) {
    std::thread::spawn(move || {
        let target = format!("TERMSRV:{}", server.host);
        let wrote_cred = !password.is_empty() && !server.username.is_empty();

        if wrote_cred {
            let _ = Command::new("cmdkey")
                .args([
                    format!("/generic:{target}"),
                    format!("/user:{}", server.username),
                    format!("/pass:{password}"),
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }

        let addr = if server.port == 3389 {
            server.host.clone()
        } else {
            format!("{}:{}", server.host, server.port)
        };
        if let Ok(mut child) = Command::new("mstsc")
            .arg(format!("/v:{addr}"))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            let _ = child.wait();
        }

        if wrote_cred {
            let _ = Command::new("cmdkey")
                .arg(format!("/delete:{target}"))
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
    });
}

/// 清理上次异常退出遗留的 TERMSRV 凭据（启动时调用）
pub fn cleanup_leftover_credentials() {
    std::thread::spawn(|| {
        let Ok(out) = Command::new("cmdkey")
            .args(["/list"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(pos) = line.find("TERMSRV:") {
                let target: String = line[pos..]
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if target.len() > "TERMSRV:".len() {
                    let _ = Command::new("cmdkey")
                        .arg(format!("/delete:{target}"))
                        .creation_flags(CREATE_NO_WINDOW)
                        .output();
                }
            }
        }
    });
}
