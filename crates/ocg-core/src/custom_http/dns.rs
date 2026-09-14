//! Connector DNS guard for IsolatedTrustedAdmin Custom / dynamic traffic.
//!
//! The validating [`reqwest::dns::Resolve`] is the resolver hyper uses to
//! connect. There is no separate preflight lookup. Sealed-adapter clients do
//! not attach this guard. An explicit or system HTTP proxy still owns
//! destination DNS; this local guard does not pin or override that hop.

use super::origin_grant::is_blocked_custom_ip;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

pub(crate) const BLOCKED_RESOLUTION: &str =
    "refusing to connect: resolved destination is a metadata or link-local address";

pub(super) fn isolated_destination_resolver() -> Arc<dyn Resolve> {
    guarded_destination_resolver(Arc::new(SystemDns))
}

pub(crate) fn guarded_destination_resolver(inner: Arc<dyn Resolve>) -> Arc<dyn Resolve> {
    Arc::new(DestinationGuardResolver { inner })
}

struct SystemDns;

impl Resolve for SystemDns {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

struct DestinationGuardResolver {
    inner: Arc<dyn Resolve>,
}

impl Resolve for DestinationGuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let inner = self.inner.resolve(name);
        Box::pin(async move {
            let addrs = inner.await?;
            let allowed = filter_resolved_destination_addrs(addrs)?;
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

pub(super) fn filter_resolved_destination_addrs(
    addrs: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    let allowed: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|addr| !is_blocked_custom_ip(addr.ip()))
        .collect();
    if allowed.is_empty() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            BLOCKED_RESOLUTION,
        )));
    }
    Ok(allowed)
}

#[cfg(test)]
pub(crate) struct DestinationResolveLog {
    names: std::sync::Mutex<Vec<String>>,
    rejections: std::sync::Mutex<Vec<String>>,
    returned: std::sync::Mutex<Vec<Vec<SocketAddr>>>,
}

#[cfg(test)]
impl Default for DestinationResolveLog {
    fn default() -> Self {
        Self {
            names: std::sync::Mutex::new(Vec::new()),
            rejections: std::sync::Mutex::new(Vec::new()),
            returned: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(test)]
impl DestinationResolveLog {
    pub(crate) fn names(&self) -> Vec<String> {
        self.names.lock().expect("resolve log").clone()
    }

    pub(crate) fn rejections(&self) -> Vec<String> {
        self.rejections.lock().expect("resolve log").clone()
    }

    pub(crate) fn returned(&self) -> Vec<Vec<SocketAddr>> {
        self.returned.lock().expect("resolve log").clone()
    }

    pub(crate) fn assert_queried(&self, host: &str) {
        let names = self.names();
        assert!(
            names.iter().any(|name| name.eq_ignore_ascii_case(host)),
            "connector never asked the DNS guard for {host}: {names:?}"
        );
    }

    pub(crate) fn assert_guard_rejected(&self, host: &str) {
        self.assert_queried(host);
        let rejections = self.rejections();
        assert!(
            rejections
                .iter()
                .any(|message| message.contains(BLOCKED_RESOLUTION)),
            "connector did not receive the DNS guard rejection for {host}: {rejections:?}"
        );
        assert!(
            self.returned().is_empty(),
            "guard rejection must not hand addresses to the connector: {:?}",
            self.returned()
        );
    }
}

#[cfg(test)]
struct RecordingResolver {
    inner: Arc<dyn Resolve>,
    log: Arc<DestinationResolveLog>,
}

#[cfg(test)]
impl Resolve for RecordingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let inner = self.inner.resolve(name);
        let log = Arc::clone(&self.log);
        Box::pin(async move {
            log.names.lock().expect("resolve log").push(host);
            match inner.await {
                Ok(addrs) => {
                    let collected: Vec<SocketAddr> = addrs.collect();
                    if collected.iter().any(|addr| is_blocked_custom_ip(addr.ip())) {
                        let message = "blocked destination addrs escaped the DNS guard";
                        log.rejections
                            .lock()
                            .expect("resolve log")
                            .push(message.to_string());
                        return Err(Box::new(io::Error::other(message)) as _);
                    }
                    log.returned
                        .lock()
                        .expect("resolve log")
                        .push(collected.clone());
                    Ok(Box::new(collected.into_iter()) as Addrs)
                }
                Err(error) => {
                    log.rejections
                        .lock()
                        .expect("resolve log")
                        .push(error.to_string());
                    Err(error)
                }
            }
        })
    }
}

#[cfg(test)]
pub(crate) fn recording_resolver(
    inner: Arc<dyn Resolve>,
    log: Arc<DestinationResolveLog>,
) -> Arc<dyn Resolve> {
    Arc::new(RecordingResolver { inner, log })
}

#[cfg(test)]
mod tests;
