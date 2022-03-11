use crate::message::{
    AlreadyConnected, ConnectNode, ConnectedNode, Encoded, FindSuccessor, FindSuccessorForFix,
    FoundSuccessor, FoundSuccessorForFix, Message, MessagePayload, MessageRelay,
    NotifiedPredecessor, NotifyPredecessor, RelayProtocol,
};
use crate::routing::{Chord, ChordAction, Did, RemoteAction};
use crate::swarm::Swarm;
use crate::types::ice_transport::IceTrickleScheme;
use anyhow::anyhow;
use anyhow::Result;
use futures::lock::Mutex;
use futures_util::pin_mut;
use futures_util::stream::StreamExt;
use std::collections::VecDeque;
use std::sync::Arc;
use web3::types::Address;

#[cfg(feature = "wasm")]
use web_sys::RtcSdpType as RTCSdpType;
#[cfg(not(feature = "wasm"))]
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;

pub struct MessageHandler {
    routing: Arc<Mutex<Chord>>,
    swarm: Arc<Mutex<Swarm>>,
}

impl MessageHandler {
    pub fn new(routing: Arc<Mutex<Chord>>, swarm: Arc<Mutex<Swarm>>) -> Self {
        Self { routing, swarm }
    }

    pub async fn send_message(&self, address: &Address, message: Message) -> Result<()> {
        // TODO: diff ttl for each message?
        let swarm = self.swarm.lock().await;
        let payload = MessagePayload::new(message, &swarm.key, None)?;
        swarm.send_message(address, payload).await
    }

    pub async fn handle_message(&self, message: &Message, prev: &Did) -> Result<()> {
        let current = &self.routing.lock().await.id;

        match message {
            Message::ConnectNode(msrp, msg) => {
                self.handle_connect_node(&msrp.record(prev), msg).await
            Message::FindSuccessor(relay, msg) => self.handle_find_successor(relay, msg).await,
            Message::FoundSuccessor(relay, msg) => self.handle_found_successor(relay, msg).await,
            Message::NotifyPredecessor(relay, msg) => {
                self.handle_notify_predecessor(relay, msg).await
            }
            Message::NotifiedPredecessor(relay, msg) => {
                self.handle_notified_predecessor(relay, msg).await
            }
            _ => Err(anyhow!("Unsupported message type")),
        }
    }

    async fn handle_connect_node(&self, msrp: &MsrpSend, msg: &ConnectNode) -> Result<()> {
        // TODO: Verify necessity based on Chord to decrease connections but make sure availablitity.

        if self.dht.id != msg.target_id {
            let next_node = match self.dht.find_successor(msg.target_id)? {
                ChordAction::Some(node) => Some(node),
                ChordAction::RemoteAction(node, _) => Some(node),
                _ => None,
            }
            .ok_or_else(|| anyhow!("Cannot find next node by dht"))?;

            return self
                .send_message(&next_node, Message::ConnectNode(msrp.clone(), msg.clone()))
                .await;
        }

        let prev_node = msrp
            .from_path
            .last()
            .ok_or_else(|| anyhow!("Cannot get node"))?;
        let answer_id = self.dht.id;

        match self.swarm.get_transport(&msg.sender_id) {
            None => {
                let trans = self.swarm.new_transport().await?;
                trans
                    .register_remote_info(msg.handshake_info.clone().try_into()?)
                    .await?;

                let handshake_info = trans
                    .get_handshake_info(self.swarm.key, RTCSdpType::Answer)
                    .await?
                    .to_string();

                self.send_message(
                    prev_node,
                    Message::ConnectNodeResponse(
                        msrp.into(),
                        ConnectNodeResponse {
                            answer_id,
                            handshake_info,
                        },
                    ),
                )
                .await?;

                trans.wait_for_connected(20, 3).await?;
                self.swarm.get_or_register(&msg.sender_id, trans);

                Ok(())
            }

            _ => {
                self.send_message(
                    prev_node,
                    Message::AlreadyConnected(msrp.into(), AlreadyConnected { answer_id }),
                )
                .await
            }
        }
    }

    async fn handle_already_connected(
        &self,
        relay: &MessageRelay,
        msg: &AlreadyConnected,
    ) -> Result<()> {
        match msrp.to_path.last() {
            Some(prev_node) => {
                self.send_message(
                    prev_node,
                    Message::AlreadyConnected(msrp.clone(), msg.clone()),
                )
                .await
            }
            None => self
                .swarm
                .get_transport(&msg.answer_id)
                .map(|_| ())
                .ok_or_else(|| anyhow!("Receive AlreadyConnected but cannot get transport")),
        }
    }

    async fn handle_connect_node_response(
        &self,
        msrp: MsrpReport,
        msg: &ConnectNodeResponse,
    ) -> Result<()> {
        match msrp.to_path.last() {
            Some(prev_node) => {
                self.send_message(
                    prev_node,
                    Message::ConnectNodeResponse(msrp.clone(), msg.clone()),
                )
                .await
            }
            None => {
                let trans = self.swarm.get_transport(&msg.answer_id).ok_or_else(|| {
                    anyhow!("Cannot get trans while handle connect node response")
                })?;

                trans
                    .register_remote_info(msg.handshake_info.clone().try_into()?)
                    .await
                    .map(|_| ())
            }
        }
    }

    async fn handle_notify_predecessor(
        &self,
        relay: &MessageRelay,
        msg: &NotifyPredecessor,
    ) -> Result<()> {
        let mut chord = self.routing.lock().await;
        chord.notify(msg.predecessor);
        let prev = relay.to_path[relay.to_path.len() - 1];
        let report = MessageRelay::new(
            relay.tx_id.clone(),
            relay.message_id.clone(),
            relay.to_path.clone(),
            relay.from_path.clone(),
            RelayProtocol::REPORT,
        );
        let message = Message::NotifiedPredecessor(
            report,
            NotifiedPredecessor {
                predecessor: chord.predecessor.clone().unwrap(),
            },
        );
        self.send_message(&(prev.into()), message).await
    }

    async fn handle_notified_predecessor(
        &self,
        relay: &MessageRelay,
        msg: &NotifiedPredecessor,
    ) -> Result<()> {
        let mut chord = self.routing.lock().await;
        assert_eq!(relay.protocol, RelayProtocol::REPORT);
        assert_eq!(relay.to_path[relay.to_path.len() - 1], chord.id);
        // if successor: predecessor is between (id, successor]
        // then update local successor
        chord.successor = msg.predecessor;
        Ok(())
    }

    async fn handle_find_successor(&self, relay: &MessageRelay, msg: &FindSuccessor) -> Result<()> {
        let chord = self.routing.lock().await;
        match chord.find_successor(msg.id) {
            Ok(action) => match action {
                ChordAction::Some(id) => {
                    let prev = relay.to_path[relay.to_path.len() - 1];
                    let report = MessageRelay::new(
                        relay.tx_id.clone(),
                        relay.message_id.clone(),
                        relay.from_path.clone(),
                        relay.to_path.clone(),
                        RelayProtocol::REPORT,
                    );
                    let message = Message::FoundSuccessor(report, FoundSuccessor { id: id });
                    self.send_message(&prev.into(), message).await
                }
                ChordAction::RemoteAction((next, RemoteAction::FindSuccessor(id))) => {
                    let mut from_path = relay.from_path.clone();
                    let mut to_path = relay.to_path.clone();
                    from_path.push_back(chord.id);
                    to_path.push_back(next);
                    let send = MessageRelay::new(
                        relay.tx_id.clone(),
                        relay.message_id.clone(),
                        from_path,
                        to_path,
                        RelayProtocol::SEND,
                    );
                    let message = Message::FindSuccessor(send, FindSuccessor { id });
                    self.send_message(&next.into(), message).await
                }
                _ => panic!(""),
            },
            Err(e) => panic!("{:?}", e),
        }
    }

    async fn handle_found_successor(
        &self,
        relay: &MessageRelay,
        msg: &FoundSuccessor,
    ) -> Result<()> {
        let mut chord = self.routing.lock().await;
        let current = chord.id;
        assert_eq!(relay.protocol, RelayProtocol::REPORT);
        let mut relay = relay.clone();
        assert_eq!(Some(current), relay.to_path.pop_back());
        relay.from_path.pop_back();
        if relay.to_path.len() > 0 {
            let prev = relay.to_path[relay.to_path.len() - 1];
            let report = MessageRelay::new(
                // tx_id and message_id need renew one later
                relay.tx_id.clone(),
                relay.message_id.clone(),
                relay.to_path.clone(),
                relay.from_path.clone(),
                RelayProtocol::REPORT,
            );
            let message = Message::FoundSuccessor(report, msg.clone());
            self.send_message(&prev.into(), message).await
        } else {
            chord.successor = msg.id;
            Ok(())
        }
    }

    pub async fn listen(&self) {
        let swarm = self.swarm.lock().await;
        let payloads = swarm.iter_messages();

        pin_mut!(payloads);

        while let Some(payload) = payloads.next().await {
            if payload.is_expired() || !payload.verify() {
                log::error!("Cannot verify msg or it's expired: {:?}", payload);
                continue;
            }

            if let Err(e) = self
                .handle_message(&payload.data, &payload.addr.into())
                .await
            {
                log::error!("Error in handle_message: {}", e);
                continue;
            }
        }
    }

    pub async fn stabilize(&self) {
        let mut chord = self.routing.lock().await;
        let (current, successor) = (chord.id, chord.successor);
        let tx_id = String::from("");
        let message_id = String::from("");
        let mut to_path = VecDeque::new();
        let mut from_path = VecDeque::new();
        to_path.push_back(successor);
        from_path.push_back(current);
        let relay = MessageRelay::new(tx_id, message_id, to_path, from_path, RelayProtocol::SEND);
        let message = Message::NotifyPredecessor(
            relay,
            NotifyPredecessor {
                predecessor: current,
            },
        );
        self.send_message(&current.into(), message).await;

        // fix fingers
        match chord.fix_fingers() {
            Ok(action) => match action {
                ChordAction::None => {
                    log::info!("")
                }
                ChordAction::RemoteAction((next, RemoteAction::FindSuccessorForFix(current))) => {
                    let tx_id = String::from("");
                    let message_id = String::from("");
                    let mut to_path = VecDeque::new();
                    let mut from_path = VecDeque::new();
                    to_path.push_back(next);
                    from_path.push_back(current);
                    let relay = MessageRelay::new(
                        tx_id,
                        message_id,
                        to_path,
                        from_path,
                        RelayProtocol::SEND,
                    );
                    let message = Message::FindSuccessorForFix(
                        relay,
                        FindSuccessorForFix { successor: current },
                    );
                    self.send_message(&next.into(), message).await;
                }
                _ => {
                    log::error!("Invalid Chord Action");
                }
            },
            Err(e) => log::error!("{:?}", e),
        }
    }
}
