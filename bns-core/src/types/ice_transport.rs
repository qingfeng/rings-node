use crate::ecc::SecretKey;
use crate::encoder::Encoded;
use crate::types::channel::Channel;
use anyhow::Result;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use web3::types::Address;

type Fut = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait IceTransport<Ch: Channel> {
    type Connection;
    type Candidate;
    type Sdp;
    type DataChannel;
    type IceConnectionState;
    type ConnectionState;
    type Msg;

    fn new(signaler: Arc<Ch>) -> Self;
    fn signaler(&self) -> Arc<Ch>;
    async fn start(&mut self, stun_addr: String) -> Result<()>;
    async fn close(&self) -> Result<()>;
    async fn ice_connection_state(&self) -> Option<Self::IceConnectionState>;

    async fn get_peer_connection(&self) -> Option<Arc<Self::Connection>>;
    async fn get_pending_candidates(&self) -> Vec<Self::Candidate>;
    async fn get_answer(&self) -> Result<Self::Sdp>;
    async fn get_offer(&self) -> Result<Self::Sdp>;
    async fn get_answer_str(&self) -> Result<String>;
    async fn get_offer_str(&self) -> Result<String>;
    async fn get_data_channel(&self) -> Option<Arc<Self::DataChannel>>;

    async fn set_local_description<T>(&self, desc: T) -> Result<()>
    where
        T: Into<Self::Sdp> + Send;
    async fn add_ice_candidate(&self, candidate: String) -> Result<()>;
    async fn set_remote_description<T>(&self, desc: T) -> Result<()>
    where
        T: Into<Self::Sdp> + Send;
}

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait IceTransportCallback<Ch: Channel>: IceTransport<Ch> {
    type OnLocalCandidateHdlrFn = Box<dyn FnMut(Option<Self::Candidate>) -> Fut + Send + Sync>;
    type OnPeerConnectionStateChangeHdlrFn =
        Box<dyn FnMut(Self::ConnectionState) -> Fut + Send + Sync>;
    type OnDataChannelHdlrFn = Box<dyn FnMut(Arc<Self::DataChannel>) -> Fut + Send + Sync>;

    async fn on_ice_candidate(&self, f: Self::OnLocalCandidateHdlrFn) -> Result<()>;
    async fn on_peer_connection_state_change(
        &self,
        f: Self::OnPeerConnectionStateChangeHdlrFn,
    ) -> Result<()>;
    async fn on_data_channel(&self, f: Self::OnDataChannelHdlrFn) -> Result<()>;

    async fn on_ice_candidate_callback(&self) -> Self::OnLocalCandidateHdlrFn;
    async fn on_peer_connection_state_change_callback(
        &self,
    ) -> Self::OnPeerConnectionStateChangeHdlrFn;
    async fn on_data_channel_callback(&self) -> Self::OnDataChannelHdlrFn;
}

#[cfg_attr(feature = "wasm", async_trait(?Send))]
#[cfg_attr(not(feature = "wasm"), async_trait)]
pub trait IceTrickleScheme<Ch: Channel>: IceTransport<Ch> {
    type SdpType;
    async fn get_handshake_info(&self, key: SecretKey, kind: Self::SdpType) -> Result<Encoded>;
    async fn register_remote_info(&self, data: Encoded) -> Result<Address>;
}
