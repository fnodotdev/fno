//! The process table: one row per process, read without exec'ing `ps` on
//! macOS. Self-contained (it names only `crate::pid`), so build.rs copies it
//! into fno, where the caller-identity walk reads it in process.

use crate::pid::Pid;

/// One row of the process table: the columns `ps -Ao
/// pid,ppid,state,etime,%cpu,rss,command` reports, read without exec'ing
/// `ps` (setuid on macOS, so a sandboxed caller's seatbelt refuses it).
#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: u32,
    pub ppid: u32,
    pub state: char,
    pub elapsed_s: u64,
    pub cpu_pct: f64,
    pub rss_kb: u64,
    pub command: String,
}

/// True while `pid` is a zombie: dead but not yet reaped by its parent, so
/// a probe keeps succeeding even though it holds no fds and serves nothing.
/// An unreadable pid reads not-zombie: this read never invents a death.
pub fn pid_is_zombie(pid: u32) -> bool {
    Pid::new(pid).is_some_and(|p| p.zombie())
}

/// The process table plus the count of pids whose row could not be read.
pub fn process_table() -> (Vec<ProcRow>, usize) {
    #[cfg(target_os = "macos")]
    {
        process_table_libproc()
    }
    #[cfg(not(target_os = "macos"))]
    {
        process_table_ps()
    }
}

/// The parent pid and short name of a process the table cannot read. The
/// table skips another uid's process (a terminal's root-owned `login`, and
/// launchd), and macOS answers this short read for any uid. A sandbox refuses
/// it, so a sandboxed caller reads `None`.
pub fn hidden_process(pid: u32) -> Option<(u32, String)> {
    Pid::new(pid).and_then(|p| p.short_bsd())
}

#[cfg(target_os = "macos")]
fn process_table_libproc() -> (Vec<ProcRow>, usize) {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return (Vec::new(), 0);
    }
    let mut pids = vec![0u32; count as usize + 64];
    let filled = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast::<libc::c_void>(),
            (pids.len() * std::mem::size_of::<u32>()) as libc::c_int,
        )
    };
    if filled <= 0 {
        return (Vec::new(), 0);
    }
    let now = epoch_now();
    let mut rows: Vec<ProcRow> = Vec::new();
    let mut unreadable = 0usize;
    for &pid in &pids[..filled as usize] {
        // A short read is a pid that exited mid-walk or a kernel task:
        // neither belongs in the table, but both count as unreadable.
        let Some(bsd) = Pid::new(pid).and_then(|p| p.bsd()) else {
            unreadable += 1;
            continue;
        };
        // Zombies never reach this line: their proc_pidinfo read is a zero
        // write and was counted unreadable above, so the sysctl-based
        // pid_is_zombie would be a dead second read per pid here.
        let zombie = bsd.pbi_status == libc::SZOMB;
        let mut rss_kb = 0u64;
        let mut usage_sum = 0i64;
        let mut state = if zombie { 'Z' } else { 'S' };
        if !zombie {
            if let Some(task) = Pid::new(pid).and_then(|p| p.task()) {
                rss_kb = task.pti_resident_size / 1024;
                let mut handles = vec![0u64; task.pti_threadnum.max(0) as usize];
                let got = Pid::new(pid)
                    .map(|p| p.thread_handles(&mut handles))
                    .unwrap_or(0);
                if got > 0 {
                    let mut any_running = false;
                    let mut any_stopped = false;
                    let mut any_uninterruptible = false;
                    for handle in &handles[..got] {
                        let Some(thread) = Pid::new(pid).and_then(|p| p.thread_info(*handle))
                        else {
                            continue;
                        };
                        usage_sum += thread.pth_cpu_usage as i64;
                        match thread.pth_run_state {
                            1 => any_running = true,
                            2 => any_stopped = true,
                            4 => any_uninterruptible = true,
                            _ => {}
                        }
                    }
                    state = if any_running {
                        'R'
                    } else if any_stopped {
                        'T'
                    } else if any_uninterruptible {
                        'U'
                    } else {
                        'S'
                    };
                }
            }
        }
        let command = argv_of(pid)
            .map(|argv| argv.join(" "))
            .unwrap_or_else(|| comm_string(&bsd.pbi_comm));
        rows.push(ProcRow {
            pid,
            ppid: bsd.pbi_ppid,
            state,
            elapsed_s: now.saturating_sub(bsd.pbi_start_tvsec),
            // pth_cpu_usage sums in TH_USAGE_SCALE (1000) units of one
            // thread; /10 reads percent, the number `ps %cpu` reports.
            cpu_pct: usage_sum as f64 / 10.0,
            rss_kb,
            command,
        });
    }
    (rows, unreadable)
}

/// The live argv of `pid` from `KERN_PROCARGS2`, `None` when unreadable.
/// Mirrors `fno::pane_argv::process_argv`; this crate never links `fno`,
/// so the read lives beside its only production caller.
#[cfg(target_os = "macos")]
pub fn argv_of(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < 4
    {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    parse_procargs2(&buf[..size])
}

/// The macOS `KERN_PROCARGS2` buffer layout: an `i32` argc, the truncated
/// exec path (NUL-terminated), NUL padding to alignment, then argc
/// NUL-terminated strings (argv[0] is the path again), then the environment.
#[cfg(target_os = "macos")]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    if buf.len() < 4 {
        return None;
    }
    let argc = i32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
    if argc == 0 {
        return None;
    }
    let mut i = 4;
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    if i >= buf.len() {
        return None;
    }
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    let mut args = Vec::with_capacity(argc);
    while args.len() < argc && i < buf.len() {
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        args.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
        i += 1;
    }
    (args.len() == argc).then_some(args)
}

#[cfg(target_os = "macos")]
pub fn comm_string(comm: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(target_os = "macos")]
fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `ps` leg for platforms where `ps` is not setuid: exec and parse the
/// same columns the native read returns.
pub fn process_table_ps() -> (Vec<ProcRow>, usize) {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-Ao", "pid,ppid,state,etime,%cpu,rss,command"])
        .output()
    else {
        return (Vec::new(), 0);
    };
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
        if let Some(row) = parse_ps_row(line) {
            rows.push(row);
        }
    }
    (rows, 0)
}

/// One `ps -Ao pid,ppid,state,etime,%cpu,rss,command` data line. Compiled
/// only where it has a caller: the Linux leg and its tests, both of which
/// are `not(target_os = "macos")`. A macOS test build otherwise compiles
/// it with no caller and deny-warnings turns the dead code into a failure.
pub fn parse_ps_row(line: &str) -> Option<ProcRow> {
    // ps right-aligns the numeric columns, so tokens must split on
    // whitespace RUNS - a per-char split yields empty fields and every
    // aligned column reads as a parse failure.
    let mut fields = line.trim().split_whitespace();
    let (Some(pid), Some(ppid), Some(state), Some(etime), Some(cpu), Some(rss)) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return None;
    };
    // A pid that fails to parse is a torn line, not pid 0; keep a real
    // pid-0 row (the swapper, where a ps dialect lists it).
    let pid = pid.parse().ok()?;
    Some(ProcRow {
        pid,
        ppid: ppid.parse().unwrap_or(0),
        state: state.chars().next().unwrap_or('?'),
        elapsed_s: parse_etime(etime).unwrap_or(0),
        cpu_pct: cpu.parse().unwrap_or(0.0),
        rss_kb: rss.parse().unwrap_or(0),
        command: fields.collect::<Vec<_>>().join(" "),
    })
}

/// Parse `ps -o etime=`: `[[DD-]HH:]MM:SS`.
///
/// `etimes` (elapsed SECONDS, no parsing) exists on Linux and NOT on macOS, so
/// the portable column is the formatted one. `None` on anything unparseable,
/// which reads as "unknown age" and therefore never reaps: an age the gate
/// cannot establish is not evidence the process is old.
pub fn parse_etime(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (days, rest) = match raw.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, raw),
    };
    let mut parts = rest.split(':').rev();
    let secs = parts.next()?.parse::<u64>().ok()?;
    let mins = parts.next()?.parse::<u64>().ok()?;
    let hours = match parts.next() {
        Some(h) => h.parse::<u64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(days * 86_400 + hours * 3_600 + mins * 60 + secs)
}

#[cfg(test)]
mod tests {
    use super::pid_is_zombie;

    #[test]
    fn pid_is_zombie_reads_an_unreaped_exit_as_zombie_and_a_live_pid_as_not() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline && !pid_is_zombie(pid) {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            pid_is_zombie(pid),
            "an exited, unwaited child reads as zombie"
        );
        assert!(
            !pid_is_zombie(std::process::id()),
            "a live pid is not zombie"
        );
        child.wait().unwrap();
    }
}
