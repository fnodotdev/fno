//! Where a message goes: the four address forms and the host spelling rule.
//! Resolving a handle to a live registry row is not this module's job.

/// A parsed destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// A bare name or session id on this machine.
    Handle(String),
    /// `name@host`, with the host already in `normalize_host` form.
    Remote { name: String, host: String },
    /// `--to-lead <scope>`: whoever holds the lead role for the scope.
    Lead(String),
    /// `--to-project <project>`: the one live peer that answers for it.
    Project(String),
}

impl Address {
    /// Parse the positional target: a handle, or `name@host`.
    pub fn parse_target(target: &str) -> Result<Address, TargetError> {
        if !target.contains('@') {
            return Ok(Address::Handle(target.to_string()));
        }
        let (name, host) = split_target(target)?;
        Ok(Address::Remote {
            name,
            host: normalize_host(&host),
        })
    }
}

/// A cross-host target that is not `<name>@<host>` with one `@` and both
/// sides nonempty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetError {
    pub target: String,
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cross-host target {:?}: the form is <name>@<host>, one @, \
both sides nonempty; a host alias in ssh_config covers user@host spellings",
            self.target
        )
    }
}

impl std::error::Error for TargetError {}

impl From<TargetError> for String {
    fn from(e: TargetError) -> String {
        e.to_string()
    }
}

/// Split `<name>@<host>` at its single `@`. Both sides nonempty; two `@`
/// refuse (an ssh user@host spelling is not carried; an ssh_config Host
/// alias covers those).
pub fn split_target(target: &str) -> Result<(String, String), TargetError> {
    let refused = || TargetError {
        target: target.to_string(),
    };
    if target.matches('@').count() != 1 {
        return Err(refused());
    }
    let (name, host) = target.split_once('@').unwrap_or(("", ""));
    if name.is_empty() || host.is_empty() {
        return Err(refused());
    }
    Ok((name.to_string(), host.to_string()))
}

/// Hosts compare equal across machines that spell themselves differently
/// (`mac2.local` from gethostname, `mac2` from a user typing). One
/// normalizer runs on write and on pull (operator ruling, 2026-10-10).
pub fn normalize_host(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.strip_suffix(".local") {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => lower,
    }
}
