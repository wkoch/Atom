#![allow(unsafe_code)]
//! Identidade de plataforma p/ `[P-CONN-2]`/`[C-AMB-1]` (spec protocol §1, ADR-019).
//!
//! ÚNICO módulo do workspace autorizado a conter `unsafe` (AGENTS.md §16,
//! exceção documentada pela ADR-019). Todo o `unsafe` aqui é chamada direta
//! de syscall/libc com pré/pós-condição auditável; nenhum outro crate do
//! kernel usa `unsafe` (`#![forbid(unsafe_code)]` continua neles).
//!
//! Justificativa (por que `unsafe` é inevitável aqui):
//! - Linux: `SO_PEERCRED` (`getsockopt`) e `PR_SET_CHILD_SUBREAPER` (`prctl`)
//!   só existem como syscalls C; `tokio::net::UnixStream::peer_cred` expõe
//!   uid/gid mas NÃO o pid do peer de forma portátil no Windows/macOS, e o
//!   subreaper precisa de `prctl` cru. `libc` é só binding FFI (sem lib C
//!   linkada além da libc do SO) — mantém cross-compile/musl estático.
//! - macOS: `LOCAL_PEERPID` (`getsockopt`) idem; árvore via `sysctl`
//!   `KERN_PROC_PID` (struct `kinfo_proc` — layout C, só via FFI).
//! - Windows: Job Object (`CreateJobObjectW`, `AssignProcessToJobObject`,
//!   `SetInformationJobObject`) só via Win32; `windows-sys` seria alternativa
//!   com dependência C pesada — aqui documentamos o contrato e o gate usa o
//!   pid do cliente até o módulo Win32 existir (fora do escopo: código usa
//!   `AF_UNIX` em toda plataforma, ver spec §1).
//!
//! Política (igual nas 3 plataformas): só recusa descendente CONFIRMADO.
//! `Unknown` (sem info do SO) → confiável + `warn` — o chamador decide.

/// Por que o peer não pôde ser identificado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerUnknown {
    /// `SO_PEERCRED`/`LOCAL_PEERPID` indisponível ou falhou.
    NoSocketInfo,
    /// Processo sumiu ou sem permissão de leitura no meio da checagem.
    Vanished,
}

/// Pid do peer de um socket unix + se a info é confiável.
/// `None` = desconhecido explícito (tratar como confiável + warn).
/// spec: [P-CONN-2]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPid {
    Known(u32),
    Unknown(PeerUnknown),
}

impl PeerPid {
    /// `Some(pid)` se conhecido, `None` se desconhecido.
    pub fn opt(self) -> Option<u32> {
        match self {
            Self::Known(p) => Some(p),
            Self::Unknown(_) => None,
        }
    }
}

/// Marca o processo atual como subreaper (`PR_SET_CHILD_SUBREAPER`): órfãos
/// de double-fork das extensões são re-apadrinhados ao daemon e continuam
/// descendentes na caminhada `/proc/<pid>/stat` (`[P-CONN-2]`, Linux).
/// Idempotente; fora do Linux é no-op. Erro de syscall vira `String`
/// (daemon loga e segue — sem info ≠ descendente).
/// spec: [P-CONN-2]
pub fn claim_subreaper() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0)` só toca flag
        // do próprio processo; args constantes válidos; sem ponteiro.
        let r = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER as libc::c_int, 1, 0, 0, 0) };
        if r != 0 {
            return Err(format!(
                "prctl(PR_SET_CHILD_SUBREAPER): {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

/// Pid do peer via credencial do socket.
///
/// - Linux: `SO_PEERCRED` (`getsockopt` no fd cru).
/// - macOS: `LOCAL_PEERPID` (`getsockopt`, melhor esforço: double-fork escapa).
/// - Outros unix sem credencial de pid: `Unknown(NoSocketInfo)`.
///
/// `fd` = fd cru do socket conectado (não toma posse, não fecha).
/// spec: [P-CONN-2]
#[cfg(unix)]
pub fn peer_pid_of_fd(fd: std::os::unix::io::RawFd) -> PeerPid {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &ucred, &len)`:
        // `fd` é do chamador (socket vivo durante o accept); `ucred` é
        // struct `libc::ucred` local com len inicializado; sem ponteiro
        // pendurado; retorno checado (`ret == 0` + len exato).
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
                &mut len as *mut libc::socklen_t,
            )
        };
        if ret == 0 && len as usize == std::mem::size_of::<libc::ucred>() {
            return PeerPid::Known(cred.pid as u32);
        }
        PeerPid::Unknown(PeerUnknown::NoSocketInfo)
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &pid, &len)`:
        // buffer `pid: pid_t` local; len inicializado; fd do chamador.
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast::<libc::c_void>(),
                &mut len as *mut libc::socklen_t,
            )
        };
        if ret == 0 {
            return PeerPid::Known(pid as u32);
        }
        PeerPid::Unknown(PeerUnknown::NoSocketInfo)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = fd;
        PeerPid::Unknown(PeerUnknown::NoSocketInfo)
    }
}

/// Ppid de `pid` p/ a caminhada de ancestralidade (`[P-CONN-2]`).
/// `None` = sumiu/sem permissão/sem info (tratar como fim da caminhada).
/// spec: [P-CONN-2]
pub fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        ppid_of_stat(&stat)
    }
    #[cfg(target_os = "macos")]
    {
        parent_of_macos(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Extrai o ppid (campo 4) de `/proc/<pid>/stat`: ancora no ÚLTIMO `)`.
/// (o `comm` pode conter espaços/parênteses). Pura p/ teste.
/// spec: [P-CONN-2]
#[cfg(target_os = "linux")]
pub fn ppid_of_stat(stat: &str) -> Option<u32> {
    let after = stat.rfind(')')?;
    let mut it = stat[after + 1..].split_whitespace();
    let _state = it.next()?;
    it.next()?.parse().ok()
}

#[cfg(target_os = "linux")]
pub use ppid_of_stat as ppid_of_proc_stat;

/// Ppid via `proc_pidinfo(PROC_PIDTBSDINFO)` (macOS, libproc). `None` = sem info.
/// struct `proc_bsdinfo` tem layout C — só via FFI, por isso vive aqui.
/// spec: [P-CONN-2]
#[cfg(target_os = "macos")]
fn parent_of_macos(pid: u32) -> Option<u32> {
    // macOS não tem `kinfo_proc` no libc (só BSDs); via libproc.
    // SAFETY: buffer `proc_bsdinfo` zerado; `pbi_ppid` lido só com `ret`
    // igual ao tamanho da struct. Residual documentado: double-fork escapa
    // da árvore (ADR-019).
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let ret = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast::<libc::c_void>(),
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        );
        if ret as usize != std::mem::size_of::<libc::proc_bsdinfo>() {
            return None;
        }
        let ppid = info.pbi_ppid;
        if ppid == 0 {
            return None;
        }
        Some(ppid)
    }
}

/// Contrato Windows p/ `[P-CONN-2]` (ADR-019): cada extensão roda num Job
/// Object sem breakaway + o gate confere o pid do cliente.
///
/// Código usa `AF_UNIX` em toda plataforma (spec §1 alinhada); sem named
/// pipe não há pid de cliente Win32 a conferir, então hoje todo peer no
/// Windows é `Unknown` (confiável + warn). Quando o transporte for named
/// pipe, este módulo ganha `create_job_for_child` / `client_pid_of_pipe`
/// via Win32 (`windows-sys`, exceção a `forbid(unsafe_code)` já prevista em
/// AGENTS.md) sem mudar o gate.
#[cfg(windows)]
pub mod job {
    /// Sem named pipe não há Job a criar; mantém o contrato p/ o host.
    pub fn assign_child_to_job(_child_pid: u32) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: [P-CONN-2]
    #[test]
    fn peer_pid_opt_semantics() {
        assert_eq!(PeerPid::Known(7).opt(), Some(7));
        assert_eq!(PeerPid::Unknown(PeerUnknown::NoSocketInfo).opt(), None);
        assert_eq!(PeerPid::Unknown(PeerUnknown::Vanished).opt(), None);
    }

    // spec: [P-CONN-2]
    #[test]
    fn claim_subreaper_idempotent() {
        assert!(claim_subreaper().is_ok());
        assert!(claim_subreaper().is_ok());
    }

    // spec: [P-CONN-2]
    #[cfg(target_os = "linux")]
    #[test]
    fn ppid_parse_tolerates_comm_parens() {
        assert_eq!(ppid_of_stat("1 (d) S 0"), Some(0));
        assert_eq!(ppid_of_stat("300 (zzz (a b) (c)) S 200 0"), Some(200));
        assert_eq!(ppid_of_stat("sem-paren"), None);
        assert_eq!(ppid_of_stat("1 (d) S abc"), None);
    }

    // spec: [P-CONN-2]
    #[cfg(target_os = "linux")]
    #[test]
    fn parent_of_self_is_some() {
        let me = std::process::id();
        assert!(parent_of(me).is_some());
    }

    // spec: [P-CONN-2]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn parent_of_unknown_off_platform() {
        assert_eq!(parent_of(1), None);
    }
}
