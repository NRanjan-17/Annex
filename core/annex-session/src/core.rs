use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use annex_crypto::{Identity, TrustStore, TrustedPeer, display_fingerprint};
use tokio::sync::oneshot;

use crate::SessionError;
use crate::events::{Event, EventSink, PeerRole};

/// How to treat peers that are not in the trust store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPolicy {
    /// Ask the user via `Event::TrustRequest` (normal operation).
    Prompt,
    /// Refuse unknown peers without asking.
    RejectUnknown,
    /// Accept and remember unknown peers. Tests and benchmarks only.
    AcceptAllInsecure,
}

const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Long-lived per-install state: identity, trust store and outstanding trust prompts.
pub struct Core {
    identity: Identity,
    trust: Mutex<TrustStore>,
    device_name: String,
    policy: TrustPolicy,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_request: AtomicU64,
}

impl Core {
    /// Loads (or creates) identity and trust store under `data_dir`.
    pub fn open(data_dir: &Path, device_name: &str, policy: TrustPolicy) -> Result<Arc<Self>, SessionError> {
        let identity = Identity::load_or_create(data_dir)?;
        let trust = TrustStore::open(&data_dir.join("trusted_peers.json"))?;
        Ok(Self::new(identity, trust, device_name, policy))
    }

    /// Fresh in-memory identity and trust store.
    pub fn ephemeral(device_name: &str, policy: TrustPolicy) -> Result<Arc<Self>, SessionError> {
        Ok(Self::new(Identity::generate()?, TrustStore::in_memory(), device_name, policy))
    }

    fn new(identity: Identity, trust: TrustStore, device_name: &str, policy: TrustPolicy) -> Arc<Self> {
        let mut name: String = device_name.chars().filter(|c| !c.is_control()).collect();
        while name.len() > annex_proto::MAX_NAME_LEN {
            name.pop();
        }
        Arc::new(Core {
            identity,
            trust: Mutex::new(trust),
            device_name: name,
            policy,
            pending: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn fingerprint(&self) -> &str {
        self.identity.fingerprint()
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn trusted_peers(&self) -> Vec<TrustedPeer> {
        self.trust.lock().unwrap().list().to_vec()
    }

    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trust.lock().unwrap().is_trusted(fingerprint)
    }

    /// Revokes a peer. Returns true if it was trusted.
    pub fn untrust(&self, fingerprint: &str) -> Result<bool, SessionError> {
        Ok(self.trust.lock().unwrap().remove(fingerprint)?)
    }

    /// Answers a pending `TrustRequest`. Returns false if the request is unknown or expired.
    pub fn respond_trust(&self, request_id: u64, accept: bool) -> bool {
        match self.pending.lock().unwrap().remove(&request_id) {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }

    /// Decides whether a peer that completed the TLS handshake may proceed.
    pub(crate) async fn authorize(
        &self,
        fingerprint: &str,
        peer_name: &str,
        peer_role: PeerRole,
        events: &EventSink,
    ) -> Result<(), SessionError> {
        if fingerprint == self.fingerprint() {
            return Err(SessionError::Violation("peer uses our own identity".into()));
        }
        if self.is_trusted(fingerprint) {
            self.trust.lock().unwrap().touch(fingerprint)?;
            return Ok(());
        }
        let accept = match self.policy {
            TrustPolicy::RejectUnknown => false,
            TrustPolicy::AcceptAllInsecure => true,
            TrustPolicy::Prompt => {
                let id = self.next_request.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = oneshot::channel();
                self.pending.lock().unwrap().insert(id, tx);
                events(Event::TrustRequest {
                    request_id: id,
                    peer_name: peer_name.to_string(),
                    peer_role,
                    fingerprint: fingerprint.to_string(),
                    fingerprint_display: display_fingerprint(fingerprint),
                });
                let answer = tokio::time::timeout(PROMPT_TIMEOUT, rx).await;
                self.pending.lock().unwrap().remove(&id);
                matches!(answer, Ok(Ok(true)))
            }
        };
        if !accept {
            return Err(SessionError::NotTrusted);
        }
        self.trust.lock().unwrap().add(fingerprint, peer_name)?;
        Ok(())
    }
}
