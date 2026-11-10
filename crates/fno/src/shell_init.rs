//! The shell-integration snippets and their CLI: what a pane's shell (or a
//! user's own rc, via `fno mux shell-init`) evals to emit OSC 133 block
//! markers and hand the pane's pinned claude theme to a claude typed by
//! hand. Moved out of `mux_cli.rs` for the shrink-only budget.

use crate::mux_cli::{EXIT_OK, EXIT_USAGE};

/// The zsh OSC 133 shell-integration snippet: `precmd` emits `D;<exit>` (the
/// just-finished command) then `A` (new prompt); `B` (command start) rides at
/// the END of `PROMPT` so it fires when the prompt finishes drawing - the
/// user's keystrokes are then echoed in the B..C window, which is what
/// rerun byte-captures as the command line (emitting B in `preexec` alongside C
/// leaves that window empty: readline has already echoed by then). `preexec`
/// emits `C` (Enter pressed, output begins). Idempotent (guarded +
/// double-eval-safe), no absolute paths (AC4-UI). A pane that eval's this
/// captures blocks (US1) and their command lines.
pub(crate) const ZSH_SHELL_INIT: &str = r#"if [ -z "${_FNO_OSC133:-}" ]; then
  _FNO_OSC133=1
  autoload -Uz add-zsh-hook
  _fno_osc133_precmd() { local e=$?; printf '\033]133;D;%s\a\033]133;A\a' "$e" }
  _fno_osc133_preexec() { printf '\033]133;C\a' }
  add-zsh-hook precmd _fno_osc133_precmd
  add-zsh-hook preexec _fno_osc133_preexec
  PROMPT="${PROMPT}%{"$'\033]133;B\a'"%}"
  # A claude typed by hand inherits the pane's pinned theme; the mux already
  # passes --settings for spawns, and a --settings the user passed wins.
  claude() {
    case "$*" in
      *--settings*) command claude "$@" ;;
      *)
        if [ -n "${FNO_CLAUDE_THEME:-}" ]; then
          command claude --settings "{\"theme\":\"$FNO_CLAUDE_THEME\"}" "$@"
        else
          command claude "$@"
        fi
        ;;
    esac
  }
fi
"#;

/// The bash OSC 133 snippet: `_fno_osc133_prompt` (LAST in `PROMPT_COMMAND`)
/// emits `D;<exit>`/`A` and arms; `B` (command start) rides at the END of `PS1`
/// (`\[...\]` non-counting) so it fires when the prompt finishes drawing - the
/// user's keystrokes then echo in the B..C window that rerun
/// byte-captures as the command line. The `DEBUG` trap emits `C` on the FIRST
/// command after the prompt, then disarms - so a pipeline emits it once, and
/// the commands inside `PROMPT_COMMAND` (and a bare Enter) do not trip it. The
/// `_fno_osc133_prompt` guard skips the trap firing on the hook itself.
/// Idempotent, no absolute paths. (A pre-existing `PROMPT_COMMAND` entry plus a
/// bare Enter is the one residual edge - a rare phantom block; zsh has none.)
pub(crate) const BASH_SHELL_INIT: &str = r#"if [ -z "${_FNO_OSC133:-}" ]; then
  _FNO_OSC133=1
  _fno_osc133_armed=""
  _fno_osc133_prompt() {
    local e=$?
    printf '\033]133;D;%s\a\033]133;A\a' "$e"
    _fno_osc133_armed=1
  }
  _fno_osc133_preexec() {
    [ -z "$_fno_osc133_armed" ] && return
    [ -n "$COMP_LINE" ] && return
    case "$BASH_COMMAND" in _fno_osc133_prompt) return ;; esac
    _fno_osc133_armed=""
    printf '\033]133;C\a'
  }
  case "$PROMPT_COMMAND" in
    *_fno_osc133_prompt*) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND;}_fno_osc133_prompt" ;;
  esac
  PS1="${PS1}"'\[\033]133;B\a\]'
  trap '_fno_osc133_preexec' DEBUG
  # A claude typed by hand inherits the pane's pinned theme; the mux already
  # passes --settings for spawns, and a --settings the user passed wins.
  claude() {
    case "$*" in
      *--settings*) command claude "$@" ;;
      *)
        if [ -n "${FNO_CLAUDE_THEME:-}" ]; then
          command claude --settings "{\"theme\":\"$FNO_CLAUDE_THEME\"}" "$@"
        else
          command claude "$@"
        fi
        ;;
    esac
  }
fi
"#;

/// `fno mux shell-init <zsh|bash>`: print an eval-able OSC 133 snippet so a
/// pane's shell emits command-block markers. A pane that never runs it still
/// captures ONE implicit block; a terminal whose shell already emits OSC 133
/// (Warp/iTerm) works with zero setup (AC4-EDGE) - the scanner does not care
/// who emits. An unsupported / missing shell is a one-line error (AC4-ERR).
pub fn shell_init(shell: Option<&str>, json: bool) -> i32 {
    let (snippet, completion) = match shell {
        Some("zsh") => (
            ZSH_SHELL_INIT,
            Some(crate::mux_cli::completion::zsh_script()),
        ),
        Some("bash") => (
            BASH_SHELL_INIT,
            Some(crate::mux_cli::completion::bash_script()),
        ),
        Some(other) => {
            eprintln!("fno mux shell-init: unsupported shell {other:?}; supported: zsh, bash");
            return EXIT_USAGE;
        }
        None => {
            eprintln!("fno mux shell-init: needs a shell argument: zsh|bash");
            return EXIT_USAGE;
        }
    };
    let completion = completion.unwrap_or_default();
    // --json keeps the envelope additive: the snippet is byte-identical, and
    // the completion generated from the typed tree rides its own field. The
    // default prints both for `eval "$(fno mux shell-init zsh)"`.
    if json {
        println!(
            "{}",
            serde_json::json!({
                "shell": shell.unwrap_or(""),
                "snippet": snippet,
                "completion": completion,
            })
        );
    } else {
        print!("{snippet}");
        print!("{completion}");
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::{BASH_SHELL_INIT, ZSH_SHELL_INIT};

    /// The shell-init hook is behavioral: with the pane's theme pinned, a
    /// plain `claude` call grows the `--settings` flag; a `--settings` the
    /// user passed wins; nothing pinned passes through untouched. bash runs
    /// everywhere this suite runs; zsh may be absent on CI images, and the
    /// zsh hook carries the same body. dash (the ubuntu /bin/sh) has no
    /// DEBUG trap, so the bash snippet runs under bash.
    #[test]
    fn the_shell_init_hook_themes_a_hand_typed_claude() {
        let scratch = std::env::temp_dir().join(format!("fno-test-hook-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("scratch bin dir");
        let fake = scratch.join("claude");
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s ' \"$@\"\n").expect("fake claude");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake claude");
        }

        let run = |snippet: &str, theme: &str, args: &str, shell: &str| {
            let mut script = String::from("set -e\n");
            script.push_str(&format!("export PATH={}:$PATH\n", scratch.display()));
            script.push_str(&format!("export FNO_CLAUDE_THEME={theme}\n"));
            script.push_str(snippet);
            script.push('\n');
            script.push_str(&format!("claude {args}\n"));
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&script)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("shell runs")
                .wait_with_output()
                .expect("shell waits");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };

        let pinned = "custom:fno:footnote-paper";
        let (ok, stdout) = run(BASH_SHELL_INIT, pinned, "--version", "bash");
        assert!(ok, "bash pinned: script failed");
        assert!(
            stdout.contains(r#"--settings {"theme":"custom:fno:footnote-paper"}"#),
            "bash pinned: no injected settings in {stdout:?}"
        );
        assert!(
            stdout.contains("--version"),
            "bash pinned: the original args survive: {stdout:?}"
        );

        // A --settings the user passed wins over the pinned theme.
        let (ok, stdout) = run(
            BASH_SHELL_INIT,
            pinned,
            "--settings '{\"theme\":\"x\"}' --version",
            "bash",
        );
        assert!(ok, "bash user-settings: script failed");
        assert!(
            stdout.contains(r#"{"theme":"x"}"#),
            "bash user-settings: the user's own settings must survive: {stdout:?}"
        );
        assert!(
            !stdout.contains("footnote-paper"),
            "bash user-settings: the pinned theme must not leak past a user --settings: {stdout:?}"
        );

        // Nothing pinned: passthrough, no injection.
        let (ok, stdout) = run(BASH_SHELL_INIT, "", "--version", "bash");
        assert!(ok, "bash unpinned: script failed");
        assert!(
            !stdout.contains("--settings"),
            "bash unpinned: passthrough must not inject: {stdout:?}"
        );

        if std::process::Command::new("zsh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .is_ok()
        {
            let (ok, stdout) = run(ZSH_SHELL_INIT, pinned, "--version", "zsh");
            assert!(ok, "zsh pinned: script failed");
            assert!(
                stdout.contains(r#"--settings {"theme":"custom:fno:footnote-paper"}"#),
                "zsh pinned: no injected settings in {stdout:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
