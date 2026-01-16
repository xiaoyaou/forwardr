use std::{collections::HashMap, net::SocketAddr, sync::OnceLock};

use bytes::Bytes;
use conlock::oneshot;

use crate::{
    error::{Error, Result},
    lock::SpinLock,
    message::{Message, MessageType},
};

mod async_impl;
// mod sync_impl;

#[derive(Clone)]
pub struct RpcClient {
    tx: tokio::sync::mpsc::Sender<(Message, oneshot::Sender<Bytes>)>,
}

impl RpcClient {
    pub async fn connect<A: Into<SocketAddr>>(addr: A) -> Result<RpcClient> {
        let tcp = tokio::net::TcpSocket::new_v4()?
            .connect(addr.into())
            .await?;
        let sender = async_impl::IoSession::run(tcp).await;
        Ok(RpcClient { tx: sender })
    }

    pub async fn send(&self, message: Message) -> Result<Bytes> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send((message, tx))
            .await
            .map_err(|_| Error::new_static("send error"))?;
        rx.await.ok_or("remote error occur".into())
    }
}

pub struct RpcServer {}

impl RpcServer {
    pub async fn new<A: Into<SocketAddr>>(addr: A) -> Result<()> {
        let tcp = tokio::net::TcpSocket::new_v4()?;
        tcp.bind(addr.into())?;
        let listener = tcp.listen(1024)?;

        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async {
                    let (mut reader, mut writer) = socket.into_split();
                    let message = async_impl::IoSession::recv_message(&mut reader).await?;
                    let mid = message.id;
                    let r = RpcServer::handle_message(message).await;
                    let message = Message {
                        id: mid,
                        message_type: MessageType::RpcResponse,
                        bytes: r,
                    };
                    async_impl::IoSession::send_mesage(&mut writer, message).await?;
                    Result::Ok(())
                });
            }
        });
        Ok(())
    }

    async fn handle_message(_: Message) -> Bytes {
        // todo dispatch message
        Bytes::new()
    }
}

pub struct RpcManager {
    clients: SpinLock<HashMap<u32, RpcClient>>,
}

impl RpcManager {
    pub fn new() -> RpcManager {
        RpcManager {
            clients: SpinLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, sid: u32, client: RpcClient) {
        self.clients.lock().insert(sid, client);
    }

    pub fn get(&self, sid: u32) -> Option<RpcClient> {
        self.clients.lock().get(&sid).cloned()
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<RpcManager> = OnceLock::new();
        INSTANCE.get_or_init(|| RpcManager::new())
    }
}
