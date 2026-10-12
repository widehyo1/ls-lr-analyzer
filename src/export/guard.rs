//! Bound database-client allocations, including those outside engine budgets.
use crate::Result;
use std::path::Path;
use std::process::Command;

pub fn client(binary: &Path, init: &Path, memory_limit: u64) -> Result<Command> {
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new(binary);
        command.args(["-batch", "-bail", "-init"]).arg(init);
        use std::os::unix::process::CommandExt;
        let address_limit = memory_limit
            .checked_mul(4)
            .and_then(|n| n.checked_add(1024 * 1024 * 1024))
            .ok_or("Database client address-space limit overflow")?;
        let address_limit = libc::rlim_t::try_from(address_limit)?;
        // SAFETY: only async-signal-safe resource-limit syscalls run between
        // fork and exec. The libc types/constants match the target's ABI.
        unsafe {
            command.pre_exec(move || {
                let mut current = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_AS, &mut current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let bounded = libc::rlimit {
                    rlim_cur: current.rlim_cur.min(address_limit),
                    rlim_max: current.rlim_max.min(address_limit),
                };
                if libc::setrlimit(libc::RLIMIT_AS, &bounded) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(command)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (binary, init, memory_limit);
        Err("Bounded database execution requires Linux RLIMIT_AS support".into())
    }
}
