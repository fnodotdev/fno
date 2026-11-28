//! The remote-hosts door's core: the address parser, the resolved-machine
//! dedup, the ssh probe and the state-word fold, as one leg. The mux TUI's
//! remote-hosts page and the desktop app's Machines panel both call these
//! fns; neither re-implements the door. The view-side parts (the page's
//! rows, the inline field, the notice plumbing) stay in `remote_hosts.rs`.

use std::time::Duration;

/// One bounded ssh probe's overall bound; the ssh connect itself gives up
/// at 5 s ([`probe_argv`]).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(12);

/// The refused path form's reason, shared by the parser and both views.
pub const REFUSED_PATH: &str = "a socket path is not a host here; type a host, an IP or user@host";

/// What one probe learned about a configured host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostState {
    Checking,
    /// ssh answered and `fno --version` ran; carries its first output line.
    Reachable(String),
    Unreachable(String),
}

/// A host the field accepts: `name`, `user@name`, `name:port`,
/// `user@name:port`, an IPv4 address, an IPv6 address (brackets optional,
/// always stored bracketed so the form re-parses), or an
/// `https://host[:port]/` URL, which gives its host. Anything ssh would
/// read as an option, and any path, is refused with the forms that work.
pub fn parse_host(input: &str) -> Result<String, String> {
    const FORMS: &str = "type a host, an IP, a host:port, or user@host";
    let input = input.trim();
    if input.is_empty() {
        return Err(FORMS.into());
    }
    if input.starts_with(['/', '~', '.']) {
        return Err(REFUSED_PATH.into());
    }
    if input.starts_with('-') {
        return Err("a host cannot start with '-'".into());
    }
    let bare = ["https://", "http://"]
        .iter()
        .find_map(|scheme| input.strip_prefix(scheme))
        .unwrap_or(input);
    let authority = bare.split(['/', '?', '#']).next().unwrap_or_default();
    let (user, rest) = match authority.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, authority),
    };
    let ok_name = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let ok_v6 = |s: &str| {
        !s.is_empty()
            && s.contains(':')
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
    };
    let port = |s: &str| -> Option<u16> { s.parse::<u16>().ok().filter(|p| *p != 0) };
    let bad_port = |s: &str| -> String {
        if s.parse::<u16>() == Ok(0) {
            "a port is 1 to 65535".into()
        } else {
            FORMS.into()
        }
    };
    // `[v6]` or `[v6]:port` reads whole; nothing else may trail the bracket.
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (v6, after) = v6.split_once(']').ok_or(FORMS)?;
        match after.strip_prefix(':') {
            Some(p) => (v6, Some(port(p).ok_or_else(|| bad_port(p))?)),
            None if after.is_empty() => (v6, None),
            None => return Err(FORMS.into()),
        }
    } else {
        match rest.rsplit_once(':') {
            // host:port, when the head names one host (no colon of its
            // own) and the tail is a real port.
            Some((host, p))
                if !host.contains(':')
                    && !p.is_empty()
                    && p.chars().all(|c| c.is_ascii_digit()) =>
            {
                (host, Some(port(p).ok_or_else(|| bad_port(p))?))
            }
            // A bare IPv6 literal reads whole: its colons never split.
            _ if ok_v6(rest) => (rest, None),
            _ => (rest, None),
        }
    };
    let v6 = host.contains(':');
    if (!v6 && !ok_name(host))
        || (v6 && !ok_v6(host))
        || host.starts_with('-')
        || user.is_some_and(|u| !ok_name(u))
    {
        return Err(FORMS.into());
    }
    let stored = match (v6, port) {
        (true, Some(p)) => format!("[{host}]:{p}"),
        (true, None) => format!("[{host}]"),
        (false, Some(p)) => format!("{host}:{p}"),
        (false, None) => host.to_string(),
    };
    Ok(match user {
        Some(user) => format!("{user}@{stored}"),
        None => stored,
    })
}

/// The host part of a stored name: the `user@` prefix, brackets and a
/// `:port` tail do not count. Feeds the resolved-machine identity.
fn host_of_stored(stored: &str) -> &str {
    let s = stored.rsplit('@').next().unwrap_or_default();
    if let Some(v6) = s.strip_prefix('[') {
        return v6.split_once(']').map_or(v6, |(v6, _)| v6);
    }
    match s.rsplit_once(':') {
        Some((host, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => host,
        _ => s,
    }
}

/// The identity of a stored name: the `user@` prefix, the case, brackets
/// and the port do not count (one machine, one identity, however typed).
/// The domain stays: two machines that share a short name across tailnet
/// domains are two machines.
pub fn host_identity(s: &str) -> String {
    host_of_stored(s).to_ascii_lowercase()
}

/// The ssh destination for a stored name: brackets stripped (ssh reads a
/// bare IPv6), and a stored port as `-p`, which [`probe_argv`] places
/// before the `--` fence.
pub fn ssh_target(stored: &str) -> (String, Option<u16>) {
    let (user, rest) = match stored.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, stored),
    };
    let prefix = user.map_or(String::new(), |u| format!("{u}@"));
    if let Some(v6) = rest.strip_prefix('[') {
        let (v6, after) = v6.split_once(']').unwrap_or((v6, ""));
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
        return (format!("{prefix}{v6}"), port);
    }
    match rest.rsplit_once(':') {
        Some((host, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (format!("{prefix}{host}"), p.parse().ok())
        }
        _ => (stored.to_string(), None),
    }
}

/// The short head of a host identity: everything before the first dot.
fn host_head(s: &str) -> String {
    host_identity(s)
        .split('.')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Whether a configured name names the same machine as one fleet-map host:
/// full names equal, or the short names matching to exactly one distinct
/// host in the set. The ambiguity guard keeps two machines that share a
/// short name from folding into one row. A dotted name claims its domain,
/// so it never rides another dotted name's head; a dot-less band claims no
/// domain, so one unique short match is the same machine (`demobox.ts.net`
/// and map row `DemoBox` fold; `linux1.a.ts.net` and `linux1.b.ts.net`
/// never do).
pub fn fold_host<'a>(configured: &str, bands: &[&'a str]) -> Option<&'a str> {
    let cfg = host_identity(configured);
    if bands.iter().any(|b| host_identity(b) == cfg) {
        return bands.iter().copied().find(|b| host_identity(b) == cfg);
    }
    // A dotted configured name answers only to a dot-less band: one that
    // claims no domain of its own. Two dotted names on different domains
    // stay two machines even when the short head matches.
    let mut hits = bands.iter().filter(|b| {
        if cfg.contains('.') {
            !host_identity(b).contains('.') && host_head(b) == host_head(&cfg)
        } else {
            host_head(b) == cfg
        }
    });
    let first = hits.next().copied()?;
    hits.next().map_or(Some(first), |_| None)
}

/// The ssh argv for one probe: the same fast options as the plan carrier, a
/// stored port as `-p`, and the host after `--` so a name can never read as
/// an option.
pub fn probe_argv(stored: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"]
        .map(String::from)
        .into();
    let (host, port) = ssh_target(stored);
    if let Some(port) = port {
        argv.push("-p".into());
        argv.push(port.to_string());
    }
    argv.push("--".into());
    argv.push(host);
    argv.push("sh -lc 'exec fno --version'".into());
    argv
}

/// One bounded ssh run: `fno --version` on the far side, in the words
/// `fno-agent attach --dial` uses for a failed hop.
pub async fn probe(host: &str) -> HostState {
    let mut command = crate::process_admission::tokio_command("ssh");
    command
        .args(probe_argv(host))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let run = crate::process_admission::tokio_output(&mut command);
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            HostState::Reachable(text.lines().next().unwrap_or("fno").trim().to_string())
        }
        Ok(Ok(out)) => {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            HostState::Unreachable(match out.status.code() {
                Some(255) => format!("could not reach {host}: {}", last.trim()),
                _ => format!(
                    "{host} answers ssh, but fno did not run there: {}",
                    last.trim()
                ),
            })
        }
        Ok(Err(e)) => HostState::Unreachable(format!("ssh did not start: {e}")),
        Err(_) => HostState::Unreachable(format!("could not reach {host}: timed out")),
    }
}

/// The beat's two words: the host is on the shared store, fresh or stale.
pub fn beat(fresh: bool, age_s: i64) -> (String, String) {
    if fresh {
        ("live".into(), "live on the shared store".into())
    } else {
        let word = format!("beat {}m ago", (age_s / 60).max(1));
        (word.clone(), format!("last {word}"))
    }
}

/// One configured host's state word and detail, the fold both surfaces
/// paint: the far band's beat wins when the host is on the shared store,
/// else the probe's answer names the state and carries the why.
pub fn state_word(band: Option<(bool, i64)>, state: &HostState) -> (String, String) {
    match (band, state) {
        (Some((fresh, age_s)), _) => beat(fresh, age_s),
        (None, HostState::Checking) => ("checking".into(), "ssh has not answered yet".into()),
        (None, HostState::Reachable(v)) => (
            "no sessions here".into(),
            format!(
                "fno runs there ({v}), but none of its sessions show up on this machine; set store.remote_url on that host"
            ),
        ),
        (None, HostState::Unreachable(why)) => ("couldn't connect".into(), why.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_names_users_urls_addresses_and_ports() {
        assert_eq!(parse_host("linux1").as_deref(), Ok("linux1"));
        assert_eq!(parse_host(" user@mac2 ").as_deref(), Ok("user@mac2"));
        // A port is content now, not a dropped suffix.
        assert_eq!(
            parse_host("https://linux1.tail1234.ts.net:8443/x").as_deref(),
            Ok("linux1.tail1234.ts.net:8443")
        );
        assert_eq!(parse_host("linux1:22").as_deref(), Ok("linux1:22"));
        // A hex-letter host keeps its port: the v6 read needs a colon in
        // the head.
        assert_eq!(parse_host("abc:22").as_deref(), Ok("abc:22"));
        assert_eq!(
            parse_host("user@linux1:2200").as_deref(),
            Ok("user@linux1:2200")
        );
        // An IPv4 reads as a plain name (the charset already admits it).
        assert_eq!(parse_host("192.168.1.20").as_deref(), Ok("192.168.1.20"));
        // IPv6 is stored bracketed, bare or bracketed in, with or without
        // a port, so the form always re-parses.
        assert_eq!(parse_host("[::1]:8443").as_deref(), Ok("[::1]:8443"));
        assert_eq!(parse_host("::1").as_deref(), Ok("[::1]"));
        assert_eq!(parse_host("2001:db8::1").as_deref(), Ok("[2001:db8::1]"));
        assert_eq!(
            parse_host("https://[2001:db8::1]:8443/").as_deref(),
            Ok("[2001:db8::1]:8443")
        );
    }

    #[test]
    fn parse_refuses_paths_options_and_junk() {
        assert_eq!(parse_host("/tmp/sock"), Err(REFUSED_PATH.to_string()));
        assert_eq!(parse_host("~/sock"), Err(REFUSED_PATH.to_string()));
        assert!(parse_host("-oProxyCommand=x").is_err());
        assert!(parse_host("a b").is_err());
        assert!(parse_host("").is_err());
        assert!(parse_host("user@-host").is_err());
        // A port is a port, not decoration: bounds hold and junk tails fail.
        assert!(parse_host("linux1:0").is_err());
        assert!(parse_host("linux1:70000").is_err());
        assert!(parse_host("linux1:abc").is_err());
        assert!(parse_host("[::1").is_err());
        assert!(parse_host("[::1]x").is_err());
        // What parse admits, the probe fences: the host rides after `--`, so
        // an ssh option can never read from it.
        let argv = probe_argv("linux1");
        let after_fence = argv.iter().skip_while(|a| *a != "--").nth(1);
        assert_eq!(after_fence.map(String::as_str), Some("linux1"));
        assert!(argv.iter().any(|a| a == "BatchMode=yes"));
    }

    #[test]
    fn ssh_target_splits_port_and_strips_brackets() {
        assert_eq!(ssh_target("linux1"), ("linux1".to_string(), None));
        assert_eq!(
            ssh_target("linux1:2222"),
            ("linux1".to_string(), Some(2222))
        );
        assert_eq!(
            ssh_target("user@linux1:22"),
            ("user@linux1".to_string(), Some(22))
        );
        assert_eq!(ssh_target("[::1]:22"), ("::1".to_string(), Some(22)));
        assert_eq!(ssh_target("[::1]"), ("::1".to_string(), None));
        // A `-p` port never rides past the `--` fence.
        let argv = probe_argv("linux1:2222");
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert!(argv.contains(&"-p".to_string()) && argv.contains(&"2222".to_string()));
        assert_eq!(argv.get(fence + 1).map(String::as_str), Some("linux1"));
    }

    #[test]
    fn folding_matches_one_machine_and_refuses_ambiguity() {
        // Full names equal, user@ and case aside.
        assert!(fold_host("user@linux1.tail1.ts.net", &["linux1.tail1.ts.net"]).is_some());
        assert!(!fold_host("linux1", &["mac2"]).is_some());
        // A dot-less name answers to one distinct full host.
        assert_eq!(
            fold_host("linux1", &["linux1.tail1.ts.net"]),
            Some("linux1.tail1.ts.net")
        );
        // Two machines sharing the short name fold to neither.
        assert!(fold_host("linux1", &["linux1.a.ts.net", "linux1.b.ts.net"]).is_none());
        // A dotted name never rides the short-name fallback across domains.
        assert!(fold_host("linux1.a.ts.net", &["linux1.b.ts.net"]).is_none());
        // A dotted name answers to a dot-less band that claims no domain.
        assert_eq!(
            fold_host("demobox.example-net.ts.net", &["DemoBox", "r2d2.b.ts.net"]),
            Some("DemoBox")
        );
        // A dotted name and a dot-less band with another head: two machines.
        assert!(fold_host("other.example-net.ts.net", &["DemoBox"]).is_none());
        // The resolved machine, not the typed text: user@, case, brackets
        // and the port do not count.
        assert!(fold_host("user@Host:22", &["host"]).is_some());
        assert!(fold_host("[::1]:8443", &["[::1]"]).is_some());
    }

    #[test]
    fn state_word_carries_the_why_and_the_beat_words() {
        // The band wins when the host is on the shared store.
        assert_eq!(state_word(Some((true, 0)), &HostState::Checking).0, "live");
        assert_eq!(
            state_word(Some((false, 360)), &HostState::Checking).0,
            "beat 6m ago"
        );
        // No band: the probe's answer names the state and carries the why.
        let (word, detail) = state_word(
            None,
            &HostState::Unreachable("could not reach h: no route".into()),
        );
        assert_eq!(word, "couldn't connect");
        assert!(detail.contains("no route"));
        let (word, _) = state_word(None, &HostState::Reachable("fno 1".into()));
        assert_eq!(word, "no sessions here");
        let (word, _) = state_word(None, &HostState::Checking);
        assert_eq!(word, "checking");
    }
}
