//! macOS process helpers: identity, parent links and listening TCP ports, all as
//! an unprivileged user. Ported from `aipm-core/src/darwin.rs`.

use std::ffi::CStr;

/// Subset of `proc_bsdinfo` this tool needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BsdInfo {
    pub ppid: i32,
    pub start_us: u64,
    pub uid: u32,
    pub tty_dev: Option<u32>,
}

/// Read `PROC_PIDTBSDINFO` for `pid`; `None` when the kernel refuses (other uid, exited).
pub fn bsd_info(pid: i32) -> Option<BsdInfo> {
    let mut bi: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `bi` is a live, properly aligned `proc_bsdinfo` and `size` is its
    // exact size, which is what the kernel writes into.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut bi).cast(), size) };
    if n != size {
        return None;
    }
    Some(BsdInfo {
        ppid: bi.pbi_ppid as i32,
        start_us: bi
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(bi.pbi_start_tvusec),
        uid: bi.pbi_uid,
        tty_dev: (bi.e_tdev != u32::MAX).then_some(bi.e_tdev),
    })
}

/// Device name for a tty device number, e.g. `ttys001`.
pub fn tty_name(dev: u32) -> String {
    // SAFETY: `devname` only reads its arguments and returns either NULL or a
    // pointer to a static NUL-terminated buffer owned by libc. Every caller of
    // this module samples from one thread at a time (the one-shot CLI or the
    // TUI's single sampler thread), so nothing can overwrite that buffer while
    // it is copied.
    let ptr = unsafe { libc::devname(dev as libc::dev_t, libc::S_IFCHR) };
    if ptr.is_null() {
        return format!("dev:{dev:#x}");
    }
    // SAFETY: non-NULL, NUL-terminated, owned by libc for the lifetime of the copy.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Every TCP socket the process owns as `(is_listening, local port)`.
fn tcp_sockets(pid: i32) -> Vec<(bool, u16)> {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::file_info::{ListFDs, ProcFDType, pidfdinfo};
    use libproc::libproc::net_info::{SocketFDInfo, SocketInfoKind, TcpSIState};
    use libproc::libproc::proc_pid::{listpidinfo, pidinfo};

    let Ok(info) = pidinfo::<BSDInfo>(pid, 0) else {
        return Vec::new();
    };
    // Slack for descriptors opened between the two calls.
    let Ok(fds) = listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize + 16) else {
        return Vec::new();
    };
    let mut sockets = Vec::new();
    for fd in fds {
        if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::Socket) {
            continue;
        }
        let Ok(sock) = pidfdinfo::<SocketFDInfo>(pid, fd.proc_fd) else {
            continue;
        };
        if !matches!(SocketInfoKind::from(sock.psi.soi_kind), SocketInfoKind::Tcp) {
            continue;
        }
        // SAFETY: `.psi` is a union; it is read only after `soi_kind` was
        // verified to be `Tcp` above, so `pri_tcp` is the initialized member.
        let tcp = unsafe { sock.psi.soi_proto.pri_tcp };
        let listening = matches!(TcpSIState::from(tcp.tcpsi_state), TcpSIState::Listen);
        sockets.push((listening, u16::from_be(tcp.tcpsi_ini.insi_lport as u16)));
    }
    sockets
}

/// TCP ports the process is listening on, sorted and deduplicated.
pub fn listening_ports(pid: i32) -> Vec<u16> {
    let mut ports: Vec<u16> = tcp_sockets(pid)
        .into_iter()
        .filter(|(listening, _)| *listening)
        .map(|(_, port)| port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bsd_info_self() {
        let info = bsd_info(std::process::id() as i32).expect("own bsd info");
        assert!(info.start_us > 0, "start_us must be set: {info:?}");
        assert!(info.ppid > 0, "ppid must be a real parent: {info:?}");
    }

    #[test]
    fn ports_self() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let port = listener.local_addr().expect("local addr").port();
        let ports = listening_ports(std::process::id() as i32);
        assert!(ports.contains(&port), "expected {port} in {ports:?}");
    }

    #[test]
    fn unknown_pid_has_no_ports() {
        assert!(listening_ports(-1).is_empty());
    }
}
