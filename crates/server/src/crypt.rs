//! Encryption at rest. Everything stored for a graph (snapshots and updates) is
//! sealed with XChaCha20-Poly1305 under a key derived from the graph's viewer
//! key, which every owner, editor and viewer key can derive (see auth.rs). The
//! database alone therefore holds no readable graph: it takes a key to open one.
//!
//! The server never stores the key. A graph is opened by a request that presents
//! a key, and the derived key lives in memory for as long as the graph does, so
//! after a restart a graph stays sealed until someone opens it with a key.
//!
//! A sealed blob is `nonce (24 bytes) || ciphertext || tag`. The graph id and the
//! kind of blob are authenticated too, so a blob copied to another graph or
//! passed off as a snapshot is refused.

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use sha2::{Digest, Sha256};

const NONCE: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Update,
    Snapshot,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Update => "update",
            Kind::Snapshot => "snapshot",
        }
    }
}

#[derive(Clone)]
pub struct Cipher {
    aead: XChaCha20Poly1305,
}

impl Cipher {
    /// The cipher for the graph whose viewer key is `viewer_key`. The domain tag keeps this
    /// key apart from the graph's stored name, which is also a hash of the viewer key.
    pub fn for_viewer_key(viewer_key: &str) -> Cipher {
        let mut h = Sha256::new();
        h.update(b"waypoint/encrypt/v1");
        h.update([0]);
        h.update(viewer_key.as_bytes());
        let key: [u8; 32] = h.finalize().into();
        Cipher { aead: XChaCha20Poly1305::new(&key.into()) }
    }

    fn aad(graph: &str, kind: Kind) -> Vec<u8> {
        format!("{}/{graph}", kind.tag()).into_bytes()
    }

    pub fn seal(&self, graph: &str, kind: Kind, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).expect("operating system randomness");
        let sealed = self
            .aead
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &Self::aad(graph, kind) })
            .expect("encryption cannot fail");
        [nonce.as_slice(), &sealed].concat()
    }

    pub fn open(&self, graph: &str, kind: Kind, sealed: &[u8]) -> Result<Vec<u8>, String> {
        if sealed.len() < NONCE {
            return Err("stored data is too short to be sealed".into());
        }
        let (nonce, body) = sealed.split_at(NONCE);
        self.aead
            .decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad: &Self::aad(graph, kind) })
            .map_err(|_| "stored data does not open with this key".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_data_opens_only_with_the_same_key_graph_and_kind() {
        let c = Cipher::for_viewer_key("wp_viewer_a");
        let sealed = c.seal("g", Kind::Update, b"secret title");
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(c.open("g", Kind::Update, &sealed).unwrap(), b"secret title");
        assert!(Cipher::for_viewer_key("wp_viewer_b").open("g", Kind::Update, &sealed).is_err());
        assert!(c.open("other", Kind::Update, &sealed).is_err());
        assert!(c.open("g", Kind::Snapshot, &sealed).is_err());
        let mut torn = sealed.clone();
        *torn.last_mut().unwrap() ^= 1;
        assert!(c.open("g", Kind::Update, &torn).is_err());
        assert_ne!(c.seal("g", Kind::Update, b"x"), c.seal("g", Kind::Update, b"x"), "nonces differ");
    }
}
