use std::collections::HashMap;

use bytes::Bytes;
use conlock::oneshot;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
};

use crate::{error::Result, lock::SpinLock, message::Message};

/// 正常架构下的Session，都应该有专门的负责处理收发数据的Worker，所以需要通过消息管道来通信
pub struct IoSession {
    // 当前消息id
    current_id: u32,
    // 待响应的请求返回管道
    map: SpinLock<HashMap<u32, oneshot::Sender<Bytes>>>,
}

impl IoSession {
    fn fetch_next_id(&mut self) -> u32 {
        self.current_id += 1;
        self.current_id
    }

    pub async fn run(socket: TcpStream) -> mpsc::Sender<(Message, oneshot::Sender<Bytes>)> {
        let (sender, receiver) = mpsc::channel(1024);
        let ios = IoSession {
            current_id: 0,
            map: SpinLock::new(HashMap::new()),
        };
        let (reader, writer) = socket.into_split();
        tokio::spawn(async move {
            // 单线程处理io读写
            if let Err(err) = tokio::try_join!(ios.run_send(writer, receiver), ios.run_recv(reader))
            {
                eprintln!("net error: {err}")
            };
        });
        sender
    }

    async fn run_send(
        &self,
        mut writer: OwnedWriteHalf,
        mut receiver: mpsc::Receiver<(Message, oneshot::Sender<Bytes>)>,
    ) -> Result<()> {
        loop {
            let Some((msg, sender)) = receiver.recv().await else {
                break Ok(());
            };
            let message_id = msg.id;
            self.map.lock().insert(message_id, sender); // 注册消息ID
            if let Err(err) = IoSession::send_mesage(&mut writer, msg).await {
                self.map.lock().remove(&message_id);
                return Err(err);
            }
        }
    }

    async fn run_recv(&self, mut reader: OwnedReadHalf) -> Result<()> {
        loop {
            let msg = IoSession::recv_message(&mut reader).await?;
            let waiter = self.map.lock().remove(&msg.id);
            if let Some(waiter) = waiter {
                // Err意味着接收端已关闭，不需要处理
                let _ = waiter.send(msg.bytes);
            } // else warning
        }
    }

    pub async fn send_mesage(writer: &mut OwnedWriteHalf, message: Message) -> Result<()> {
        writer.write_u32(message.id).await?;
        writer.write_u8(message.message_type as u8).await?;
        writer.write_u32(message.bytes.len().try_into()?).await?;
        writer.write_all(&message.bytes).await?;
        Ok(())
    }

    pub async fn recv_message(reader: &mut OwnedReadHalf) -> Result<Message> {
        let id = reader.read_u32().await?;
        let message_type = reader.read_u8().await?.try_into()?;
        // FIXME: 限制缓冲区大小
        let size = reader.read_u32().await? as usize;
        // SAFETY: u8任意数值均有效，且后续会被重新写入
        let mut buffer = unsafe { Box::new_uninit_slice(size).assume_init() };
        reader.read_exact(&mut buffer).await?;
        Ok(Message {
            id,
            message_type,
            bytes: Bytes::from(buffer),
        })
    }
}
