//! The program a client command runs to reach another client command.
//!
//! The client dispatch runs in process inside `fno`, so `current_exe()` there
//! names `fno`, which answers nested spellings, not the client's flat words.
//! Inside `fno` the client program is the `fno-agent` entry beside it: `fno`
//! answers a client argv when started under that name. Anywhere else (the
//! `fno-agent` binary, the daemon, a test harness) it is the running program,
//! as before.

use std::path::{Path, PathBuf};

/// The `argv[0]` under which `fno` runs a client argv verbatim.
pub const CLIENT_ARG0: &str = "fno-agent";

/// `current_exe()` for a client re-exec: see the module docs.
pub fn current() -> std::io::Result<PathBuf> {
    std::env::current_exe().map(|exe| for_exe(&exe))
}

/// The client program for a process running `exe`.
pub fn for_exe(exe: &Path) -> PathBuf {
    if exe.file_stem().is_some_and(|stem| stem == "fno") {
        exe.with_file_name(CLIENT_ARG0)
    } else {
        exe.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::for_exe;
    use std::path::Path;

    #[test]
    fn inside_fno_the_client_is_its_sibling_and_elsewhere_the_program_itself() {
        assert_eq!(
            for_exe(Path::new("/opt/bin/fno")),
            Path::new("/opt/bin/fno-agent")
        );
        for exe in [
            "/opt/bin/fno-agent",
            "/opt/bin/fno-agent-daemon",
            "/t/deps/x-123",
        ] {
            assert_eq!(for_exe(Path::new(exe)), Path::new(exe));
        }
    }
}
