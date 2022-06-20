use std::sync::Arc;
use std::time::Duration;

use futures::lock::Mutex;
use rings_core::async_trait;
use rings_core::message::MessageCallback;
use rings_core::prelude::web3::contract::tokens::Tokenizable;
use rings_node::prelude::rings_core;
use rings_node::prelude::*;
use rings_node::processor::*;
use wasm_bindgen_test::*;
// wasm_bindgen_test_configure!(run_in_browser);

fn new_processor() -> Processor {
    let key = SecretKey::random();

    let (auth, new_key) = SessionManager::gen_unsign_info(key.address(), None, None).unwrap();
    let sig = key.sign(&auth.to_string().unwrap()).to_vec();
    let session = SessionManager::new(&sig, &auth, &new_key);
    let swarm = Arc::new(Swarm::new(
        "stun://stun.l.google.com:19302",
        key.address(),
        session,
    ));

    let dht = Arc::new(Mutex::new(PeerRing::new(key.address().into())));
    let msg_handler = MessageHandler::new(dht, swarm.clone());
    (swarm, Arc::new(msg_handler)).into()
}

struct MsgCallbackStruct {
    msgs: Arc<Mutex<Vec<String>>>,
}

#[async_trait(?Send)]
impl MessageCallback for MsgCallbackStruct {
    async fn custom_message(
        &self,
        handler: &MessageHandler,
        _ctx: &MessagePayload<Message>,
        msg: &MaybeEncrypted<CustomMessage>,
    ) {
        let msg = handler.decrypt_msg(msg).unwrap();
        let text = String::from_utf8(msg.0).unwrap();
        console_log!("msg received: {}", text);
        let mut msgs = self.msgs.try_lock().unwrap();
        msgs.push(text);
    }

    async fn builtin_message(&self, _handler: &MessageHandler, _ctx: &MessagePayload<Message>) {}
}

#[wasm_bindgen_test]
async fn test_processor_handshake_and_msg() {
    super::setup_log();
    let p1 = new_processor();
    let p2 = new_processor();
    let (transport_1, offer) = p1.create_offer().await.unwrap();

    let pendings_1 = p1.swarm.pending_transports().await.unwrap();
    assert_eq!(pendings_1.len(), 1);
    assert_eq!(
        pendings_1.get(0).unwrap().id.to_string(),
        transport_1.id.to_string()
    );

    let (_transport_2, answer) = p2.answer_offer(offer.to_string().as_str()).await.unwrap();
    let peer = p1
        .accept_answer(
            transport_1.id.to_string().as_str(),
            answer.to_string().as_str(),
        )
        .await
        .unwrap();
    assert!(peer.transport.id.eq(&transport_1.id), "transport not same");
    // transport_1
    //     .connect_success_promise()
    //     .await
    //     .unwrap()
    //     .await
    //     .unwrap();
    // transport_2
    //     .connect_success_promise()
    //     .await
    //     .unwrap()
    //     .await
    //     .unwrap();

    // assert!(
    //     transport_1.is_connected().await,
    //     "transport_1 not connected"
    // );
    // assert!(
    //     transport_2.is_connected().await,
    //     "transport_2 not connected"
    // );

    let msgs1: Arc<Mutex<Vec<String>>> = Default::default();
    let msgs2: Arc<Mutex<Vec<String>>> = Default::default();
    let callback1 = Box::new(MsgCallbackStruct {
        msgs: msgs1.clone(),
    });
    let callback2 = Box::new(MsgCallbackStruct {
        msgs: msgs2.clone(),
    });

    let test_text1 = "test1";
    let test_text2 = "test2";

    let p1_addr = p1.address().into_token().to_string();
    let p2_addr = p2.address().into_token().to_string();
    console_log!("p1_addr: {}", p1_addr);
    console_log!("p2_addr: {}", p2_addr);

    console_log!("listen");
    p1.msg_handler.set_callback(callback1).await;
    p1.msg_handler.clone().listen().await;

    p2.msg_handler.set_callback(callback2).await;
    p2.msg_handler.clone().listen().await;

    p2.send_message(p1_addr.as_str(), test_text2.as_bytes())
        .await
        .unwrap();

    // fluvio_wasm_timer::Delay::new(Duration::from_secs(1))
    //     .await
    //     .unwrap();

    p1.send_message(p2_addr.as_str(), test_text1.as_bytes())
        .await
        .unwrap();

    console_log!("send_done");

    fluvio_wasm_timer::Delay::new(Duration::from_secs(2))
        .await
        .unwrap();

    console_log!("check received");

    let mut msgs2 = msgs2.try_lock().unwrap();
    let got_msg2 = msgs2.pop().unwrap();
    assert!(
        got_msg2.eq(test_text1),
        "msg received, expect {}, got {}",
        test_text1,
        got_msg2
    );

    let mut msgs1 = msgs1.try_lock().unwrap();
    let got_msg1 = msgs1.pop().unwrap();
    assert!(
        got_msg1.eq(test_text2),
        "msg received, expect {}, got {}",
        test_text2,
        got_msg1
    );
}
