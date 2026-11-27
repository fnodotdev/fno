//! The fno-agent client dispatch, run in process: every client word the
//! front door used to exec as the `fno-agent` binary runs here. A runnable
//! `$FNO_AGENT_BIN` still takes the argv as an exec, because the front-door
//! tests stub the client through it.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;

/// Run the client argv `argv` (no program name) and exit with its code.
pub fn run(label: &str, argv: Vec<OsString>) -> ! {
    // An override that names this program (the `fno-agent` link) would exec
    // straight back here, so it runs in process instead.
    if let Some(bin) =
        crate::digest_overlay::fno_agent_override().filter(|bin| !is_this_program(bin))
    {
        exec(label, &bin, argv);
    }
    run_here(label, argv)
}

/// [`run`] with no override: the entry `fno` takes when started under the
/// client's name.
pub fn run_here(label: &str, argv: Vec<OsString>) -> ! {
    let mut args = Vec::with_capacity(argv.len());
    for arg in argv {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                eprintln!(
                    "{label}: an argument is not UTF-8: {}",
                    arg.to_string_lossy()
                );
                std::process::exit(2);
            }
        }
    }
    for (key, value) in client_env() {
        std::env::set_var(key, value);
    }
    fno_agent::client_cli::main(args)
}

fn is_this_program(bin: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    let here = std::env::current_exe().ok();
    canonical(bin).is_some_and(|bin| here.as_deref().and_then(canonical) == Some(bin))
}

/// Exec `bin` with the client argv, carrying the env the in-process path
/// sets. Exits 1 when the exec fails.
pub fn exec(label: &str, bin: &Path, argv: Vec<OsString>) -> ! {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(argv);
    cmd.envs(client_env());
    let error = cmd.exec();
    eprintln!("{label}: could not run {}: {error}", bin.display());
    std::process::exit(1)
}

/// The usage row's start stamp, and the socket dir the client's mux calls
/// read from one variable a user shell never exports.
fn client_env() -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = crate::cli_usage::exec_env()
        .into_iter()
        .map(|(key, value)| (key, OsString::from(value)))
        .collect();
    if std::env::var_os("FNO_MUX_DIR")
        .filter(|v| !v.is_empty())
        .is_none()
    {
        env.push(("FNO_MUX_DIR", crate::proto::mux_dir().into_os_string()));
    }
    env
}
