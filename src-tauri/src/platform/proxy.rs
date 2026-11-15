//! Operating-system proxy discovery and effective-proxy resolution.
//!
//! Discovery is confined to the platform layer. Shared code resolves the
//! configured mode through [`resolve_proxy`] and never branches on the target
//! operating system. A PAC or WPAD configuration, or a configuration that
//! cannot be read, is reported as unavailable with a stable reason code that
//! never contains a configuration value.

use crate::config::{ProxyMode, ProxyType, Settings};

/// The operating system's HTTP and HTTPS proxy configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemProxyDiscovery {
    /// The system exposes explicit HTTP and/or HTTPS proxy endpoints.
    Proxy {
        /// Effective HTTP proxy URL, when one is configured.
        http: Option<String>,
        /// Effective HTTPS proxy URL, when one is configured.
        https: Option<String>,
    },
    /// Discovery is impossible without evaluating a PAC script or WPAD.
    Unavailable {
        /// Stable reason code such as `pac_configured` or `unreadable_configuration`.
        reason: String,
    },
}

/// Injectable operating-system proxy discovery.
pub trait SystemProxySource: Send + Sync {
    /// Read the current system proxy configuration.
    fn discover(&self) -> SystemProxyDiscovery;
}

/// The real platform source backed by the operating system configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlatformSystemProxySource;

impl SystemProxySource for PlatformSystemProxySource {
    fn discover(&self) -> SystemProxyDiscovery {
        #[cfg(target_os = "macos")]
        {
            discover_macos()
        }
        #[cfg(target_os = "windows")]
        {
            discover_windows()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            SystemProxyDiscovery::Unavailable {
                reason: "unsupported_platform".to_string(),
            }
        }
    }
}

/// A concrete proxy that can be handed to the HTTP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProxy {
    /// Proxy URL without embedded userinfo.
    pub url: String,
    /// Optional basic-authentication username.
    pub username: Option<String>,
    /// Optional basic-authentication password.
    pub password: Option<String>,
}

/// The effective proxy selection for one settings snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyResolution {
    /// Connect without a proxy.
    Direct,
    /// Route traffic through the resolved proxy.
    Proxy(ResolvedProxy),
    /// The configured proxy cannot be used, so traffic connects directly.
    Unavailable {
        /// Stable reason code, never a configuration value.
        reason: String,
    },
}

/// Resolve the effective proxy for `settings`.
///
/// Direct mode ignores both the system configuration and stored credentials.
/// Manual mode uses the configured type, host, and port plus an optional
/// username/password pair. System mode prefers the HTTPS endpoint, falls back
/// to the HTTP endpoint, and never carries credentials.
pub fn resolve_proxy(
    settings: &Settings,
    credentials: Option<(&str, &str)>,
    source: &dyn SystemProxySource,
) -> ProxyResolution {
    match settings.proxy_mode {
        ProxyMode::Direct => ProxyResolution::Direct,
        ProxyMode::Manual => resolve_manual(settings, credentials),
        ProxyMode::System => resolve_system(source),
    }
}

fn resolve_manual(settings: &Settings, credentials: Option<(&str, &str)>) -> ProxyResolution {
    let Some(host) = settings
        .proxy_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
    else {
        return ProxyResolution::Unavailable {
            reason: "manual_proxy_host_missing".to_string(),
        };
    };
    let Some(port) = settings.proxy_port.filter(|port| *port > 0) else {
        return ProxyResolution::Unavailable {
            reason: "manual_proxy_port_missing".to_string(),
        };
    };

    let scheme = match settings.proxy_type {
        ProxyType::Http => "http",
        ProxyType::Https => "https",
        ProxyType::Socks5 => "socks5",
    };
    let (username, password) = match credentials {
        Some((username, password)) if !username.is_empty() => {
            (Some(username.to_string()), Some(password.to_string()))
        }
        _ => (None, None),
    };

    ProxyResolution::Proxy(ResolvedProxy {
        url: format!("{scheme}://{host}:{port}"),
        username,
        password,
    })
}

fn resolve_system(source: &dyn SystemProxySource) -> ProxyResolution {
    match source.discover() {
        SystemProxyDiscovery::Unavailable { reason } => ProxyResolution::Unavailable { reason },
        SystemProxyDiscovery::Proxy { http, https } => {
            match preferred_system_url(https, http) {
                Some(url) => ProxyResolution::Proxy(ResolvedProxy {
                    url,
                    username: None,
                    password: None,
                }),
                // A readable configuration with no endpoint uses a direct
                // connection rather than reporting a failure.
                None => ProxyResolution::Direct,
            }
        }
    }
}

fn preferred_system_url(https: Option<String>, http: Option<String>) -> Option<String> {
    https
        .filter(|value| !value.trim().is_empty())
        .or_else(|| http.filter(|value| !value.trim().is_empty()))
}

#[cfg(target_os = "macos")]
fn discover_macos() -> SystemProxyDiscovery {
    use std::process::Command;

    let output = match Command::new("scutil").arg("--proxy").output() {
        Ok(output) if output.status.success() => output,
        _ => return unreadable(),
    };
    parse_scutil(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_scutil(text: &str) -> SystemProxyDiscovery {
    use std::collections::HashMap;

    let mut fields = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim().to_string(), unquote(value.trim()));
        }
    }

    let enabled = |key: &str| fields.get(key).is_some_and(|value| value == "1");
    let pac_url = fields
        .get("ProxyAutoConfigURLString")
        .is_some_and(|value| !value.is_empty());
    if pac_url || enabled("ProxyAutoConfigEnable") || enabled("ProxyAutoDiscoveryEnable") {
        return SystemProxyDiscovery::Unavailable {
            reason: "pac_configured".to_string(),
        };
    }

    let http = endpoint(&fields, enabled("HTTPEnable"), "HTTPProxy", "HTTPPort");
    let https = endpoint(&fields, enabled("HTTPSEnable"), "HTTPSProxy", "HTTPSPort");

    SystemProxyDiscovery::Proxy { http, https }
}

#[cfg(target_os = "macos")]
fn endpoint(
    fields: &std::collections::HashMap<String, String>,
    enabled: bool,
    host_key: &str,
    port_key: &str,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let host = fields.get(host_key)?.trim();
    let port = fields.get(port_key)?.trim();
    if host.is_empty() || port.is_empty() {
        return None;
    }
    Some(format!("http://{host}:{port}"))
}

#[cfg(target_os = "windows")]
fn discover_windows() -> SystemProxyDiscovery {
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let output = match Command::new("reg").args(["query", KEY]).output() {
        Ok(output) if output.status.success() => output,
        _ => return unreadable(),
    };
    parse_windows_registry(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "windows")]
fn parse_windows_registry(text: &str) -> SystemProxyDiscovery {
    use std::collections::HashMap;

    let mut values = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(_kind)) = (parts.next(), parts.next()) else {
            continue;
        };
        let value = parts.collect::<Vec<_>>().join(" ");
        values.insert(name.to_string(), value);
    }

    let pac_url = values
        .get("AutoConfigURL")
        .is_some_and(|value| !value.is_empty());
    let wpad = values
        .get("AutoDetect")
        .is_some_and(|value| value.eq_ignore_ascii_case("0x1"));
    if pac_url || wpad {
        return SystemProxyDiscovery::Unavailable {
            reason: "pac_configured".to_string(),
        };
    }

    let proxy_enabled = values
        .get("ProxyEnable")
        .is_some_and(|value| value.eq_ignore_ascii_case("0x1"));
    if !proxy_enabled {
        return SystemProxyDiscovery::Proxy {
            http: None,
            https: None,
        };
    }

    let (http, https) = values
        .get("ProxyServer")
        .map(|value| parse_windows_proxy_server(value))
        .unwrap_or_default();
    SystemProxyDiscovery::Proxy { http, https }
}

#[cfg(target_os = "windows")]
fn parse_windows_proxy_server(value: &str) -> (Option<String>, Option<String>) {
    let mut http = None;
    let mut https = None;
    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((scheme, address)) if scheme.eq_ignore_ascii_case("http") => {
                http = normalize_proxy_address(address);
            }
            Some((scheme, address)) if scheme.eq_ignore_ascii_case("https") => {
                https = normalize_proxy_address(address);
            }
            Some(_) => {}
            None => http = normalize_proxy_address(part),
        }
    }
    (http, https)
}

#[cfg(target_os = "windows")]
fn normalize_proxy_address(address: &str) -> Option<String> {
    let address = address.trim();
    if address.is_empty() {
        return None;
    }
    if address.contains("://") {
        Some(address.to_string())
    } else {
        Some(format!("http://{address}"))
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn unreadable() -> SystemProxyDiscovery {
    SystemProxyDiscovery::Unavailable {
        reason: "unreadable_configuration".to_string(),
    }
}

#[cfg(target_os = "macos")]
fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_string()
}
