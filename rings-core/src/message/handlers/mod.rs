use std::sync::Arc;

use async_recursion::async_recursion;
use async_trait::async_trait;

use super::CustomMessage;
use super::MaybeEncrypted;
use super::Message;
use super::MessagePayload;
use super::OriginVerificationGen;
use super::PayloadSender;
use crate::dht::Did;
use crate::dht::PeerRing;
use crate::err::Error;
use crate::err::Result;
use crate::session::SessionManager;
use crate::swarm::Swarm;

/// Operator and Handler for Connection
pub mod connection;
/// Operator and Handler for CustomMessage
pub mod custom;
/// Operator and handler for DHT stablization
pub mod stabilization;
/// Operator and Handler for Storage
pub mod storage;
/// Operator and Handler for SubRing
pub mod subring;

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait MessageCallback {
    async fn custom_message(
        &self,
        handler: &MessageHandler,
        ctx: &MessagePayload<Message>,
        msg: &MaybeEncrypted<CustomMessage>,
    );
    async fn builtin_message(&self, handler: &MessageHandler, ctx: &MessagePayload<Message>);
}

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait MessageValidator {
    async fn validate(
        &self,
        handler: &MessageHandler,
        ctx: &MessagePayload<Message>,
    ) -> Option<String>;
}

#[cfg(not(feature = "wasm"))]
pub type CallbackFn = Box<dyn MessageCallback + Send + Sync>;

#[cfg(feature = "wasm")]
pub type CallbackFn = Box<dyn MessageCallback>;

#[cfg(not(feature = "wasm"))]
pub type ValidatorFn = Box<dyn MessageValidator + Send + Sync>;

#[cfg(feature = "wasm")]
pub type ValidatorFn = Box<dyn MessageValidator>;

#[derive(Clone)]
pub struct MessageHandler {
    dht: Arc<PeerRing>,
    swarm: Arc<Swarm>,
    callback: Arc<Option<CallbackFn>>,
    validator: Arc<Option<ValidatorFn>>,
}

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait HandleMsg<T> {
    async fn handle(&self, ctx: &MessagePayload<Message>, msg: &T) -> Result<()>;
}

impl MessageHandler {
    pub fn new(
        swarm: Arc<Swarm>,
        callback: Option<CallbackFn>,
        validator: Option<ValidatorFn>,
    ) -> Self {
        Self {
            dht: swarm.dht(),
            swarm,
            callback: Arc::new(callback),
            validator: Arc::new(validator),
        }
    }

    async fn invoke_callback(&self, payload: &MessagePayload<Message>) -> Result<()> {
        if let Some(ref cb) = *self.callback {
            match payload.data {
                Message::CustomMessage(ref msg) => {
                    if self.dht.id == payload.relay.destination {
                        cb.custom_message(self, payload, msg).await
                    }
                }
                _ => cb.builtin_message(self, payload).await,
            };
        }
        Ok(())
    }

    async fn validate(&self, payload: &MessagePayload<Message>) -> Result<()> {
        if let Some(ref v) = *self.validator {
            v.validate(self, payload)
                .await
                .map(|info| Err(Error::InvalidMessage(info)))
                .unwrap_or(Ok(()))?;
        };
        Ok(())
    }

    pub fn decrypt_msg(&self, msg: &MaybeEncrypted<CustomMessage>) -> Result<CustomMessage> {
        let key = self.swarm.session_manager().session_key()?;
        let (decrypt_msg, _) = msg.to_owned().decrypt(key)?;
        Ok(decrypt_msg)
    }

    #[cfg_attr(feature = "wasm", async_recursion(?Send))]
    #[cfg_attr(not(feature = "wasm"), async_recursion)]
    pub async fn handle_payload(&self, payload: &MessagePayload<Message>) -> Result<()> {
        #[cfg(test)]
        {
            println!("{} got msg {}", self.swarm.did(), &payload.data);
        }
        tracing::trace!("NEW MESSAGE: {}", &payload.data);

        self.validate(payload).await?;

        match &payload.data {
            Message::JoinDHT(ref msg) => self.handle(payload, msg).await,
            Message::LeaveDHT(ref msg) => self.handle(payload, msg).await,
            Message::ConnectNodeSend(ref msg) => self.handle(payload, msg).await,
            Message::ConnectNodeReport(ref msg) => self.handle(payload, msg).await,
            Message::AlreadyConnected(ref msg) => self.handle(payload, msg).await,
            Message::FindSuccessorSend(ref msg) => self.handle(payload, msg).await,
            Message::FindSuccessorReport(ref msg) => self.handle(payload, msg).await,
            Message::NotifyPredecessorSend(ref msg) => self.handle(payload, msg).await,
            Message::NotifyPredecessorReport(ref msg) => self.handle(payload, msg).await,
            Message::SearchVNode(ref msg) => self.handle(payload, msg).await,
            Message::FoundVNode(ref msg) => self.handle(payload, msg).await,
            Message::StoreVNode(ref msg) => self.handle(payload, msg).await,
            Message::CustomMessage(ref msg) => self.handle(payload, msg).await,
            Message::MultiCall(ref msg) => {
                for message in msg.messages.iter().cloned() {
                    let payload = MessagePayload::new(
                        message,
                        self.swarm.session_manager(),
                        OriginVerificationGen::Stick(payload.origin_verification.clone()),
                        payload.relay.clone(),
                    )?;
                    self.handle_payload(&payload).await.unwrap_or(());
                }
                Ok(())
            }
            x => Err(Error::MessageHandlerUnsupportMessageType(format!(
                "{:?}",
                x
            ))),
        }?;

        if let Err(e) = self.invoke_callback(payload).await {
            tracing::warn!("invoke callback error: {}", e);
        }

        Ok(())
    }

    /// This method is required because web-sys components is not `Send`
    /// which means a listening loop cannot running concurrency.
    pub async fn listen_once(&self) -> Option<MessagePayload<Message>> {
        if let Some(payload) = self.swarm.poll_message().await {
            if !payload.verify() {
                tracing::error!("Cannot verify msg or it's expired: {:?}", payload);
            }
            if let Err(e) = self.handle_payload(&payload).await {
                tracing::error!("Error in handle_message: {}", e);
                #[cfg(test)]
                {
                    println!("Error in handle_message: {}", e);
                }
            }
            Some(payload)
        } else {
            None
        }
    }
}

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
impl PayloadSender<Message> for MessageHandler {
    fn session_manager(&self) -> &SessionManager {
        self.swarm.session_manager()
    }

    async fn do_send_payload(&self, did: Did, payload: MessagePayload<Message>) -> Result<()> {
        self.swarm.do_send_payload(did, payload).await
    }
}

#[cfg(not(feature = "wasm"))]
mod listener {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::pin_mut;
    use futures::stream::StreamExt;

    use super::MessageHandler;
    use crate::types::message::MessageListener;

    #[async_trait]
    impl MessageListener for MessageHandler {
        async fn listen(self: Arc<Self>) {
            let payloads = self.swarm.iter_messages().await;
            pin_mut!(payloads);
            while let Some(payload) = payloads.next().await {
                if !payload.verify() {
                    tracing::error!("Cannot verify msg or it's expired: {:?}", payload);
                    continue;
                }
                if let Err(e) = self.handle_payload(&payload).await {
                    tracing::error!("Error in handle_message: {}", e);
                    continue;
                }
            }
        }
    }
}

#[cfg(feature = "wasm")]
mod listener {
    use std::sync::Arc;

    use async_trait::async_trait;
    use wasm_bindgen_futures::spawn_local;

    use super::MessageHandler;
    use crate::poll;
    use crate::types::message::MessageListener;

    #[async_trait(?Send)]
    impl MessageListener for MessageHandler {
        async fn listen(self: Arc<Self>) {
            let handler = Arc::clone(&self);
            let func = move || {
                let handler = handler.clone();
                spawn_local(Box::pin(async move {
                    handler.listen_once().await;
                }));
            };
            poll!(func, 1000);
        }
    }
}

#[cfg(not(feature = "wasm"))]
#[cfg(test)]
pub mod tests {
    use futures::lock::Mutex;
    use tokio::time::sleep;
    use tokio::time::Duration;

    use super::*;
    use crate::dht::Did;
    use crate::ecc::SecretKey;
    use crate::message::MessageHandler;
    use crate::tests::default::prepare_node;
    use crate::tests::manually_establish_connection;
    use crate::types::message::MessageListener;

    #[derive(Clone)]
    struct MessageCallbackInstance {
        #[allow(clippy::type_complexity)]
        handler_messages: Arc<Mutex<Vec<(Did, Vec<u8>)>>>,
    }

    #[tokio::test]
    async fn test_custom_message_handling() -> Result<()> {
        let key1 = SecretKey::random();
        let key2 = SecretKey::random();

        let (did1, _dht1, swarm1, _handler1, _path1) = prepare_node(key1).await;
        let (did2, _dht2, swarm2, _handler2, _path2) = prepare_node(key2).await;

        manually_establish_connection(&swarm1, &swarm2).await?;

        #[async_trait]
        impl MessageCallback for MessageCallbackInstance {
            async fn custom_message(
                &self,
                handler: &MessageHandler,
                ctx: &MessagePayload<Message>,
                msg: &MaybeEncrypted<CustomMessage>,
            ) {
                let decrypted_msg = handler.decrypt_msg(msg).unwrap();
                self.handler_messages
                    .lock()
                    .await
                    .push((ctx.addr, decrypted_msg.0));
                println!("{:?}, {:?}, {:?}", ctx, ctx.addr, msg);
            }

            async fn builtin_message(
                &self,
                _handler: &MessageHandler,
                ctx: &MessagePayload<Message>,
            ) {
                println!("{:?}, {:?}", ctx, ctx.addr);
            }
        }

        let msg_callback1 = MessageCallbackInstance {
            handler_messages: Arc::new(Mutex::new(vec![])),
        };
        let msg_callback2 = MessageCallbackInstance {
            handler_messages: Arc::new(Mutex::new(vec![])),
        };
        let cb1: CallbackFn = Box::new(msg_callback1.clone());
        let cb2: CallbackFn = Box::new(msg_callback2.clone());

        let handler1 = swarm1.create_message_handler(Some(cb1), None);
        let handler2 = swarm2.create_message_handler(Some(cb2), None);

        handler1
            .send_direct_message(
                Message::custom("Hello world 1 to 2 - 1".as_bytes(), None)?,
                did2,
            )
            .await
            .unwrap();

        handler1
            .send_direct_message(
                Message::custom("Hello world 1 to 2 - 2".as_bytes(), None)?,
                did2,
            )
            .await
            .unwrap();

        handler2
            .send_direct_message(
                Message::custom("Hello world 2 to 1 - 1".as_bytes(), None)?,
                did1,
            )
            .await
            .unwrap();

        handler1
            .send_direct_message(
                Message::custom("Hello world 1 to 2 - 3".as_bytes(), None)?,
                did2,
            )
            .await
            .unwrap();

        handler2
            .send_direct_message(
                Message::custom("Hello world 2 to 1 - 2".as_bytes(), None)?,
                did1,
            )
            .await
            .unwrap();

        tokio::spawn(async { Arc::new(handler1).listen().await });
        tokio::spawn(async { Arc::new(handler2).listen().await });

        sleep(Duration::from_secs(5)).await;

        assert_eq!(msg_callback1.handler_messages.lock().await.as_slice(), &[
            (did2, "Hello world 2 to 1 - 1".as_bytes().to_vec()),
            (did2, "Hello world 2 to 1 - 2".as_bytes().to_vec())
        ]);

        assert_eq!(msg_callback2.handler_messages.lock().await.as_slice(), &[
            (did1, "Hello world 1 to 2 - 1".as_bytes().to_vec()),
            (did1, "Hello world 1 to 2 - 2".as_bytes().to_vec()),
            (did1, "Hello world 1 to 2 - 3".as_bytes().to_vec())
        ]);

        Ok(())
    }
}
