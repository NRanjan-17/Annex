//! The rendezvous client against the real Worker. Needs a running server, so it's skipped unless
//! `ANNEX_RENDEZVOUS` is set, e.g.:
//!
//!     (cd server/rendezvous && npx wrangler dev --port 8788) &
//!     ANNEX_RENDEZVOUS=ws://127.0.0.1:8788 cargo test -p annex-session --test rendezvous

use std::net::SocketAddr;

use annex_session::{SessionError, rendezvous};

fn server() -> Option<String> {
    std::env::var("ANNEX_RENDEZVOUS").ok()
}

#[tokio::test]
async fn host_and_viewer_swap_addresses_once_then_the_relay() {
    let Some(server) = server() else { return };
    let host_addrs: Vec<SocketAddr> =
        vec!["203.0.113.10:47800".parse().unwrap(), "[2001:db8::5]:47800".parse().unwrap()];
    let viewer_addrs: Vec<SocketAddr> = vec!["198.51.100.20:51234".parse().unwrap()];

    let waiting = rendezvous::host(&server, "Dev's Mac", &host_addrs, false).await.unwrap();
    let code = waiting.code();
    assert!(code.len() == 7 && code.as_bytes()[3] == b'-', "{code}");
    let host = tokio::spawn(waiting.wait());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let joined = rendezvous::join(&server, &code.to_lowercase(), "Office PC", &viewer_addrs).await.unwrap();
    assert_eq!(joined.peer, rendezvous::Peer { name: "Dev's Mac".into(), addrs: host_addrs });
    let mut matched = host.await.unwrap().unwrap();
    assert_eq!(matched.peer, rendezvous::Peer { name: "Office PC".into(), addrs: viewer_addrs.clone() });

    // The direct connection "failed": the viewer asks for the relay, the host answers with its
    // relayed address. (No TURN key on a dev server, so no login from ours.)
    let relayed: SocketAddr = "198.51.100.5:61000".parse().unwrap();
    let viewer = tokio::spawn(joined.ask_relay());
    assert_eq!(matched.relay_request().await.unwrap(), Some(None));
    matched.answer_relay(Some(relayed)).await.unwrap();
    assert_eq!(viewer.await.unwrap().unwrap(), Some(vec![relayed]));

    let again = rendezvous::join(&server, &code, "Someone else", &viewer_addrs).await;
    assert!(matches!(again, Err(SessionError::Rejected(r)) if r == "unknown_code"));
}
