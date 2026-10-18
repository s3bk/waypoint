//! WebSocket sync using the y-protocol, so the stock `y-websocket` client
//! works against it. The server is a peer that holds the canonical copy:
//! document updates from one client are applied to it and relayed to the
//! others, and awareness (presence, live drag previews) is relayed without
//! being stored.

use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use tokio::sync::broadcast;
use yrs::{
    ReadTxn, Transact, Update,
    encoding::read::Cursor,
    sync::{Message, MessageReader, protocol::SyncMessage},
    updates::{
        decoder::{Decode, DecoderV1},
        encoder::Encode,
    },
};

use crate::rooms::Room;

/// A frame to relay to every connection except `from`.
#[derive(Clone)]
pub struct Relay {
    pub from: u64,
    pub frame: Vec<u8>,
}

fn send(msg: Message) -> WsMessage {
    WsMessage::Binary(msg.encode_v1().into())
}

/// `can_write` is false for viewers: they receive everything but their document updates are dropped.
pub async fn serve(mut socket: WebSocket, room: Arc<Room>, conn: u64, can_write: bool) {
    let mut relayed = room.relay.subscribe();

    // Ask the client for what we lack. The client also sends its own step 1,
    // which we answer below.
    let sv = room.graph.doc().transact().state_vector();
    if socket.send(send(Message::Sync(SyncMessage::SyncStep1(sv)))).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(WsMessage::Binary(data))) = incoming else {
                    match incoming {
                        Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                        _ => continue, // ignore text/ping frames
                    }
                };
                let Some(replies) = handle_frame(&room, conn, &data, can_write) else { continue };
                room.repair();
                room.assign_keys(); // new nodes get their short key and everyone hears about it
                room.maybe_compact();
                for reply in replies {
                    if socket.send(send(reply)).await.is_err() {
                        return;
                    }
                }
            }
            relay = relayed.recv() => match relay {
                Ok(r) if r.from != conn => {
                    if socket.send(WsMessage::Binary(r.frame.into())).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Missed updates: resend the full state. Updates are idempotent.
                    let full = room.graph.encode_state();
                    if socket.send(send(Message::Sync(SyncMessage::Update(full)))).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// Process one frame from a client. Returns messages to send back to it, and
/// relays document updates and awareness to the other connections.
fn handle_frame(room: &Room, conn: u64, data: &[u8], can_write: bool) -> Option<Vec<Message>> {
    let mut decoder = DecoderV1::new(Cursor::new(data));
    let mut replies = Vec::new();
    for msg in MessageReader::new(&mut decoder) {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("conn {conn}: bad message: {e}");
                return Some(replies);
            }
        };
        match msg {
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                let update = room.graph.doc().transact().encode_state_as_update_v1(&sv);
                replies.push(Message::Sync(SyncMessage::SyncStep2(update)));
            }
            Message::Sync(SyncMessage::SyncStep2(_) | SyncMessage::Update(_)) if !can_write => {}
            Message::Sync(SyncMessage::SyncStep2(bytes) | SyncMessage::Update(bytes)) => {
                match Update::decode_v1(&bytes) {
                    Ok(update) => {
                        if let Err(e) = room.graph.doc().transact_mut().apply_update(update) {
                            eprintln!("conn {conn}: update rejected: {e}");
                            continue;
                        }
                        let frame = Message::Sync(SyncMessage::Update(bytes)).encode_v1();
                        let _ = room.relay.send(Relay { from: conn, frame });
                    }
                    Err(e) => eprintln!("conn {conn}: bad update: {e}"),
                }
            }
            Message::Awareness(update) => {
                let frame = Message::Awareness(update).encode_v1();
                let _ = room.relay.send(Relay { from: conn, frame });
            }
            Message::AwarenessQuery | Message::Auth(_) | Message::Custom(..) => {}
        }
    }
    Some(replies)
}
