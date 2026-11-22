//! The phone-sink base-url check: an ntfy `server.yml` whose `base-url`
//! drifted from the live Tailscale name or from the `[[attention.sinks]]`
//! url breaks iOS instant delivery silently (sink health still reads 200).
//! One rename did exactly that on 2026-11-08. `fno config doctor` flags
//! the drift; nothing here refuses.

use crate::trusted_config::{Rows, TrustedFile, TrustedKey};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::PathBuf;

/// The advisory reporter: prints nothing on a machine without the ntfy
/// server installed, one ok line when aligned, one flag per mismatch.
pub fn report(out: &mut String) {
    let Some(yaml_path) = server_yml_candidates().into_iter().find(|p| p.is_file()) else {
        return;
    };
    let Ok(yaml) = std::fs::read_to_string(&yaml_path) else {
        return;
    };
    let Some(base_url) = extract_base_url(&yaml) else {
        let _ = writeln!(
            out,
            "\nphone sink: {} holds no base-url: iOS instant delivery needs one",
            yaml_path.display()
        );
        return;
    };
    let trusted = TrustedFile::from_env();
    // An unreadable cwd has no project file to report.
    let cwd = std::env::current_dir().unwrap_or_default();
    let project = crate::digest_overlay::project_config_files(&cwd);
    for note in trusted.notes(TrustedKey::AttentionSinks, &project) {
        out.push_str("\nphone sink: ");
        out.push_str(&note);
        out.push('\n');
    }
    let sink_hosts = ntfy_sink_hosts(&trusted);
    if sink_hosts.is_empty() {
        return;
    }
    let base_host = host_of(&base_url).unwrap_or("");
    let ts = tailscale_dns_name();
    let mut flagged = false;
    for url_host in &sink_hosts {
        if url_host != &base_host {
            let _ = writeln!(
                out,
                "\nphone sink: server.yml base-url host {base_host} differs from the sink url host {url_host}: iOS instant delivery keys on base-url plus topic, so pushes stop while sink health still reads 200. Point both at the live Tailscale name (docs/status-fanout.md)."
            );
            flagged = true;
        }
    }
    if let Some(ts) = &ts {
        if *ts != base_host {
            let _ = writeln!(
                out,
                "\nphone sink: server.yml base-url host {base_host} differs from the live Tailscale name {ts}: a machine rename leaves the phone subscribed under the old name. Derive base-url from the live Tailscale name."
            );
            flagged = true;
        }
    }
    if !flagged {
        let _ = writeln!(
            out,
            "\nphone sink: base-url matches the ntfy sink url and the live Tailscale name"
        );
    }
}

/// The ntfy sink url hosts the runtime uses: the trusted file's. Deduped.
fn ntfy_sink_hosts(trusted: &TrustedFile) -> Vec<String> {
    let Rows::List(rows) = trusted.rows(TrustedKey::AttentionSinks) else {
        return Vec::new();
    };
    let mut hosts: Vec<String> = vec![];
    for row in &rows {
        if row.get("type").and_then(toml::Value::as_str) != Some("ntfy") {
            continue;
        }
        let Some(url) = row.get("url").and_then(toml::Value::as_str) else {
            continue;
        };
        if let Some(host) = host_of(url) {
            let host = host.to_string();
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

fn server_yml_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/etc/ntfy/server.yml"),
        PathBuf::from("/usr/local/etc/ntfy/server.yml"),
        PathBuf::from("/etc/ntfy/server.yml"),
    ]
}

/// The top-level `base-url:` scalar: the key opens the line, the value
/// quoted or bare, a trailing comment stripped.
fn extract_base_url(yaml: &str) -> Option<String> {
    for line in yaml.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("base-url:") else {
            continue;
        };
        let v = rest.trim().trim_matches('"').trim_matches('\'');
        let v = v.split('#').next().unwrap_or("").trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// The host of a url: between `://` and the next `/`.
fn host_of(url: &str) -> Option<&str> {
    let after = url.split("://").nth(1)?;
    Some(after.split('/').next().unwrap_or(""))
}

/// The DNS name a `tailscale status --json` payload carries: `Self.DNSName`
/// with the trailing dot stripped. None when absent. Shared with the UI's
/// add-host example, which shows this machine's real tailnet name.
pub(crate) fn tailscale_dns_name_of(status_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(status_json).ok()?;
    v.get("Self")?
        .get("DNSName")?
        .as_str()
        .map(|s| s.trim_end_matches('.').to_string())
}

/// The machine's live Tailscale DNS name. None when the CLI is absent or
/// unreadable, so the comparison skips instead of blocking the report.
fn tailscale_dns_name() -> Option<String> {
    let child = std::process::Command::new("tailscale")
        .arg("status")
        .arg("--json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let out = child.wait_with_output().ok()?;
    tailscale_dns_name_of(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The base-url extractor: quoted and bare values, a trailing comment,
    // and no match on nested keys.
    #[test]
    fn extract_base_url_reads_the_top_level_scalar() {
        let quoted =
            "listen-http: \"127.0.0.1:2586\"\nbase-url: \"https://demo-box.example.ts.net:8443\"\n";
        assert_eq!(
            extract_base_url(quoted).as_deref(),
            Some("https://demo-box.example.ts.net:8443")
        );
        let bare = "base-url: https://demo-box.example.ts.net:8443 # the tailnet name\n";
        assert_eq!(
            extract_base_url(bare).as_deref(),
            Some("https://demo-box.example.ts.net:8443")
        );
        assert_eq!(
            extract_base_url("upstream-base-url: https://ntfy.sh\n"),
            None
        );
        // The host read rides this test: between the scheme and the first
        // slash.
        assert_eq!(
            host_of("https://demo-box.ts.net:8443/topic"),
            Some("demo-box.ts.net:8443")
        );
        assert_eq!(host_of("no-scheme"), None);
    }

    // The DNS-name read: Self.DNSName with the trailing dot stripped, and
    // None when the payload holds no name.
    #[test]
    fn tailscale_dns_name_strips_the_dot() {
        let payload = r#"{"Self":{"DNSName":"demo-box.example.ts.net.","Online":true}}"#;
        assert_eq!(
            tailscale_dns_name_of(payload),
            Some("demo-box.example.ts.net".to_string())
        );
        assert_eq!(tailscale_dns_name_of(r#"{"Self":{}}"#), None);
        assert_eq!(tailscale_dns_name_of("not json"), None);
    }
}
