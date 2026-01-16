use std::{
    collections::HashMap,
    io::{Read, Write},
    mem::MaybeUninit,
    usize,
};

use bytes::Bytes;
use conlock::oneshot;
use std::{net::TcpStream, sync::mpsc};

use crate::{error::Result, lock::SpinLock, message::Message};

/// 正常架构下的Session，都应该有专门的负责处理收发数据的Worker，所以需要通过消息管道来通信
struct IoSession {
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

    pub fn run(socket: TcpStream) -> mpsc::Sender<(Message, oneshot::Sender<Bytes>)> {
        let ios = IoSession {
            current_id: 0,
            map: SpinLock::new(HashMap::new()),
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            std::thread::scope(|scope| {
                let writer = socket.try_clone().unwrap();
                scope.spawn(|| ios.run_send(writer, receiver));
                scope.spawn(|| ios.run_recv(socket));
            })
        });
        sender
    }

    fn run_send(
        &self,
        mut writer: TcpStream,
        receiver: mpsc::Receiver<(Message, oneshot::Sender<Bytes>)>,
    ) -> Result<()> {
        loop {
            let Ok((msg, sender)) = receiver.recv() else {
                break Ok(());
            };
            let message_id = msg.id;
            self.map.lock().insert(message_id, sender); // 注册消息ID
            if let Err(err) = IoSession::send_mesage(&mut writer, msg) {
                self.map.lock().remove(&message_id);
                return Err(err);
            }
        }
    }

    fn run_recv(&self, mut reader: TcpStream) -> Result<()> {
        loop {
            let msg = IoSession::recv_message(&mut reader)?;
            let waiter = self.map.lock().remove(&msg.id);
            if let Some(waiter) = waiter {
                // Err意味着接收端已关闭，不需要处理
                let _ = waiter.send(msg.bytes);
            } // else warning
        }
    }

    fn send_mesage(writer: &mut TcpStream, message: Message) -> Result<()> {
        let header = match (
            message.id.to_be_bytes(),
            message.message_type,
            u32::try_from(message.bytes.len())?.to_be_bytes(),
        ) {
            ([a, b, c, d], t, [w, x, y, z]) => [a, b, c, d, t as u8, w, x, y, z],
        };
        writer.write_all(&header)?;
        writer.write_all(&message.bytes)?;
        Ok(())
    }

    fn recv_message(reader: &mut TcpStream) -> Result<Message> {
        let id = u32::read_from(reader)?;
        let message_type = u8::read_from(reader)?.try_into()?;
        // FIXME: 限制缓冲区大小
        let size = u32::read_from(reader)? as usize;
        // SAFETY: u8任意数值均有效，且后续会被重新写入
        let mut buffer = unsafe { Box::new_uninit_slice(size).assume_init() };
        reader.read_exact(&mut buffer)?;
        Ok(Message {
            id,
            message_type,
            bytes: Bytes::from(buffer),
        })
    }
}

trait ReadUnsigned: Sized {
    fn read_from<T: Read>(reader: &mut T) -> Result<Self>;
}

macro_rules! impl_unsigned_for {
    ($( $num:ty ), *) => {
        $(
          impl ReadUnsigned for $num {

              fn read_from<T: Read>(reader:  &mut T) -> Result<Self> {
                  let mut buf = MaybeUninit::uninit();
                  let ptr: &mut [u8; size_of::<$num>()] = unsafe { &mut *buf.as_mut_ptr() };
                  reader.read_exact(ptr)?;
                  Ok(Self::from_be_bytes(unsafe { buf.assume_init() }))
              }
          }
        )*
    };
}

impl_unsigned_for!(u8, u16, u32, u64);
