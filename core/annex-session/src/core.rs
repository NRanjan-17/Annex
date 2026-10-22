use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_crypto::{Identity, KeyStorage, TrustStore, TrustedPeer};
use tokio::sync::oneshot;

use crate::SessionError;

/// How to treat peers that are not in the trust store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPolicy {
    /// Require PIN pairing (normal operation).
    Pair,
    /// Refuse unknown peers without pairing.
    RejectUnknown,
    /// Accept and remember unknown peers without pairing. Tests and benchmarks only.
    AcceptAllInsecure,
}

/// Failed pairing attempts allowed within [`FAILURE_WINDOW`] before pairing is locked.
pub const MAX_PAIRING_FAILURES: usize = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Long-lived per-install state: identity, trust store, pending user input, pairing lockout.
pub struct Core {
    identity: Identity,
    trust: Mutex<TrustStore>,
    device_name: String,
    policy: TrustPolicy,
    /// Outstanding requests waiting for the user: a PIN (viewer) or a cancel (host).
    pending: Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>,
    next_request: AtomicU64,
    failures: Mutex<VecDeque<Instant>>,
}

impl Core {
    /// Loads (or creates) identity and trust store under `data_dir`; the private key goes to the
    /// platform's secure store (Keychain / DPAPI).
    pub fn open(data_dir: &Path, device_name: &str, policy: TrustPolicy) -> Result<Arc<Self>, SessionError> {
        Self::open_with(data_dir, device_name, policy, KeyStorage::platform_default())
    }

    pub fn open_with(
        data_dir: &Path,
        device_name: &str,
        policy: TrustPolicy,
        storage: KeyStorage,
    ) -> Result<Arc<Self>, SessionError> {
        let identity = Identity::load_or_create_with(data_dir, storage)?;
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
            failures: Mutex::new(VecDeque::new()),
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

    pub fn policy(&self) -> TrustPolicy {
        self.policy
    }

    pub fn trusted_peers(&self) -> Vec<TrustedPeer> {
        self.trust.lock().unwrap().list().to_vec()
    }

    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trust.lock().unwrap().is_trusted(fingerprint)
    }

    /// Whether we can skip pairing with this peer.
    pub(crate) fn knows(&self, fingerprint: &str) -> bool {
        self.policy == TrustPolicy::AcceptAllInsecure || self.is_trusted(fingerprint)
    }

    pub(crate) fn trust_peer(&self, fingerprint: &str, name: &str) -> Result<(), SessionError> {
        Ok(self.trust.lock().unwrap().add(fingerprint, name)?)
    }

    pub(crate) fn touch(&self, fingerprint: &str) {
        let _ = self.trust.lock().unwrap().touch(fingerprint);
    }

    /// Host side: lets a trusted device control this computer's mouse and keyboard (or stops it).
    /// Running sessions pick the change up within about a second. Returns false if not trusted.
    pub fn set_allow_input(&self, fingerprint: &str, allow: bool) -> Result<bool, SessionError> {
        Ok(self.trust.lock().unwrap().set_allow_input(fingerprint, allow)?)
    }

    /// Whether a device may control this computer. Never true for untrusted devices, including
    /// under `AcceptAllInsecure`.
    pub fn allows_input(&self, fingerprint: &str) -> bool {
        self.trust.lock().unwrap().get(fingerprint).is_some_and(|p| p.allow_input)
    }

    /// Revokes a peer. Returns true if it was trusted.
    pub fn untrust(&self, fingerprint: &str) -> Result<bool, SessionError> {
        Ok(self.trust.lock().unwrap().remove(fingerprint)?)
    }

    /// Answers a `pairing_pin_required` request with the PIN the user typed.
    /// Returns false if the request is unknown or already answered.
    pub fn submit_pin(&self, request_id: u64, pin: &str) -> bool {
        self.answer(request_id, Some(pin.to_string()))
    }

    /// Cancels a pending pairing (either side).
    pub fn cancel_request(&self, request_id: u64) -> bool {
        self.answer(request_id, None)
    }

    fn answer(&self, request_id: u64, value: Option<String>) -> bool {
        match self.pending.lock().unwrap().remove(&request_id) {
            Some(tx) => tx.send(value).is_ok(),
            None => false,
        }
    }

    pub(crate) fn register_request(&self) -> (u64, PendingRequest<'_>) {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        (id, PendingRequest { core: self, id, rx: Some(rx) })
    }

    /// If pairing is locked after repeated failures, how long until it unlocks.
    pub(crate) fn pairing_locked_for(&self) -> Option<Duration> {
        let mut f = self.failures.lock().unwrap();
        let now = Instant::now();
        while f.front().is_some_and(|t| now.duration_since(*t) > FAILURE_WINDOW) {
            f.pop_front();
        }
        (f.len() >= MAX_PAIRING_FAILURES).then(|| FAILURE_WINDOW - now.duration_since(f[0]))
    }

    pub(crate) fn record_pairing_failure(&self) {
        self.failures.lock().unwrap().push_back(Instant::now());
    }
}

/// A registered request; unregisters itself when dropped.
pub(crate) struct PendingRequest<'a> {
    core: &'a Core,
    id: u64,
    rx: Option<oneshot::Receiver<Option<String>>>,
}

impl PendingRequest<'_> {
    /// Waits for the user's answer. `None` = cancelled (or the answer channel was dropped).
    pub async fn answer(&mut self) -> Option<String> {
        match self.rx.as_mut() {
            Some(rx) => rx.await.ok().flatten(),
            None => std::future::pending().await,
        }
    }
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.core.pending.lock().unwrap().remove(&self.id);
    }
}
