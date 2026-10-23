// Copyright 2023-2026 Divy Srivastava <dj.srivastava23@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! _fastwebsockets-futures_ is a vendored adaptation of fastwebsockets that uses
//! `futures-lite::io` traits instead of `tokio::io`.
//!
//! It provides a minimal, fast WebSocket frame parser and server implementation.

mod close;
mod error;
mod fragment;
mod frame;
mod mask;

use bytes::Buf;

use bytes::BytesMut;
use std::future::Future;

use futures_lite::io::AsyncRead;
use futures_lite::io::AsyncReadExt;
use futures_lite::io::AsyncWrite;
use futures_lite::io::AsyncWriteExt;

pub use crate::close::CloseCode;
pub use crate::error::WebSocketError;
pub use crate::fragment::FragmentCollectorRead;
pub use crate::frame::Frame;
pub use crate::frame::OpCode;
pub use crate::frame::Payload;
pub use crate::mask::unmask;

#[derive(Copy, Clone, PartialEq)]
pub enum Role {
  Server,
  Client,
}

pub(crate) struct WriteHalf {
  role: Role,
  closed: bool,
  vectored: bool,
  auto_apply_mask: bool,
  writev_threshold: usize,
  write_buffer: Vec<u8>,
}

pub(crate) struct ReadHalf {
  role: Role,
  auto_apply_mask: bool,
  auto_close: bool,
  auto_pong: bool,
  writev_threshold: usize,
  max_message_size: usize,
  buffer: BytesMut,
}

pub struct WebSocketRead<S> {
  stream: S,
  read_half: ReadHalf,
}

pub struct WebSocketWrite<S> {
  stream: S,
  write_half: WriteHalf,
}

/// Create a split `WebSocketRead`/`WebSocketWrite` pair from a stream that has already completed the WebSocket handshake.
pub fn after_handshake_split<R, W>(
  read: R,
  write: W,
  role: Role,
) -> (WebSocketRead<R>, WebSocketWrite<W>)
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
  (
    WebSocketRead {
      stream: read,
      read_half: ReadHalf::after_handshake(role),
    },
    WebSocketWrite {
      stream: write,
      write_half: WriteHalf::after_handshake(role),
    },
  )
}

impl<'f, S> WebSocketRead<S> {
  /// Consumes the `WebSocketRead` and returns the underlying stream.
  #[inline]
  pub(crate) fn into_parts_internal(self) -> (S, ReadHalf) {
    (self.stream, self.read_half)
  }

  pub fn set_writev_threshold(&mut self, threshold: usize) {
    self.read_half.writev_threshold = threshold;
  }

  /// Sets whether to automatically close the connection when a close frame is received. When set to `false`, the application will have to manually send close frames.
  ///
  /// Default: `true`
  pub fn set_auto_close(&mut self, auto_close: bool) {
    self.read_half.auto_close = auto_close;
  }

  /// Sets whether to automatically send a pong frame when a ping frame is received.
  ///
  /// Default: `true`
  pub fn set_auto_pong(&mut self, auto_pong: bool) {
    self.read_half.auto_pong = auto_pong;
  }

  /// Sets the maximum message size in bytes. If a message is received that is larger than this, the connection will be closed.
  ///
  /// Default: 64 MiB
  pub fn set_max_message_size(&mut self, max_message_size: usize) {
    self.read_half.max_message_size = max_message_size;
  }

  /// Sets whether to automatically apply the mask to the frame payload.
  ///
  /// Default: `true`
  pub fn set_auto_apply_mask(&mut self, auto_apply_mask: bool) {
    self.read_half.auto_apply_mask = auto_apply_mask;
  }

  /// Reads a frame from the stream.
  /// Canceling while this future waits for input bytes leaves the partial frame
  /// buffered for the next call.
  pub async fn read_frame<R, E>(
    &mut self,
    send_fn: &mut impl FnMut(Frame<'f>) -> R,
  ) -> Result<Frame<'f>, WebSocketError>
  where
    S: AsyncRead + Unpin,
    E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    R: Future<Output = Result<(), E>>,
  {
    loop {
      let (res, obligated_send) =
        self.read_half.read_frame_inner(&mut self.stream).await;
      if let Some(frame) = obligated_send {
        let res = send_fn(frame).await;
        res.map_err(|e| WebSocketError::SendError(e.into()))?;
      }
      if let Some(frame) = res? {
        break Ok(frame);
      }
    }
  }
}

impl<'f, S> WebSocketWrite<S> {
  /// Sets whether to use vectored writes. This option does not guarantee that vectored writes will be always used.
  ///
  /// Default: `true`
  pub fn set_writev(&mut self, vectored: bool) {
    self.write_half.vectored = vectored;
  }

  pub fn set_writev_threshold(&mut self, threshold: usize) {
    self.write_half.writev_threshold = threshold;
  }

  /// Sets whether to automatically apply the mask to the frame payload.
  ///
  /// Default: `true`
  pub fn set_auto_apply_mask(&mut self, auto_apply_mask: bool) {
    self.write_half.auto_apply_mask = auto_apply_mask;
  }

  pub fn is_closed(&self) -> bool {
    self.write_half.closed
  }

  pub async fn write_frame(
    &mut self,
    frame: Frame<'f>,
  ) -> Result<(), WebSocketError>
  where
    S: AsyncWrite + Unpin,
  {
    self.write_half.write_frame(&mut self.stream, frame).await
  }

  pub async fn flush(&mut self) -> Result<(), WebSocketError>
  where
    S: AsyncWrite + Unpin,
  {
    flush(&mut self.stream).await
  }
}

#[inline]
async fn flush<S>(stream: &mut S) -> Result<(), WebSocketError>
where
  S: AsyncWrite + Unpin,
{
  stream.flush().await.map_err(WebSocketError::IoError)
}

const MAX_HEADER_SIZE: usize = 14;

/// Reads from `stream` into a stack buffer, extending `buf` with the bytes read.
/// Returns the number of bytes read, or 0 if EOF.
async fn read_buf<R: AsyncRead + Unpin>(
  stream: &mut R,
  buf: &mut BytesMut,
) -> std::io::Result<usize> {
  let mut tmp = [0u8; 8192];
  let n = stream.read(&mut tmp).await?;
  buf.extend_from_slice(&tmp[..n]);
  Ok(n)
}

impl ReadHalf {
  pub fn after_handshake(role: Role) -> Self {
    let buffer = BytesMut::with_capacity(8192);

    Self {
      role,
      auto_apply_mask: true,
      auto_close: true,
      auto_pong: true,
      writev_threshold: 1024,
      max_message_size: 64 << 20,
      buffer,
    }
  }

  /// Attempt to read a single frame from the incoming stream, returning any send obligations if
  /// `auto_close` or `auto_pong` are enabled. Callers to this function are obligated to send the
  /// frame in the latter half of the tuple if one is specified, unless the write half of this socket
  /// has been closed.
  ///
  /// XXX: Do not expose this method to the public API.
  pub(crate) async fn read_frame_inner<'a, S>(
    &mut self,
    stream: &mut S,
  ) -> (Result<Option<Frame<'a>>, WebSocketError>, Option<Frame<'a>>)
  where
    S: AsyncRead + Unpin,
  {
    let mut frame = match self.parse_frame_header(stream).await {
      Ok(frame) => frame,
      Err(e) => return (Err(e), None),
    };

    if self.role == Role::Server && self.auto_apply_mask {
      frame.unmask()
    };

    match frame.opcode {
      OpCode::Close if self.auto_close => {
        match frame.payload.len() {
          0 => {}
          1 => return (Err(WebSocketError::InvalidCloseFrame), None),
          _ => {
            let code = close::CloseCode::from(u16::from_be_bytes(
              frame.payload[0..2].try_into().unwrap(),
            ));

            if simdutf8::basic::from_utf8(&frame.payload[2..]).is_err() {
              return (Err(WebSocketError::InvalidUTF8), None);
            };

            if !code.is_allowed() {
              return (
                Err(WebSocketError::InvalidCloseCode),
                Some(Frame::close(1002, &frame.payload[2..])),
              );
            }
          }
        };

        let obligated_send = Frame::close_raw(frame.payload.to_owned().into());
        (Ok(Some(frame)), Some(obligated_send))
      }
      OpCode::Ping if self.auto_pong => {
        (Ok(None), Some(Frame::pong(frame.payload)))
      }
      OpCode::Text => (Ok(Some(frame)), None),
      _ => (Ok(Some(frame)), None),
    }
  }

  async fn parse_frame_header<'a, S>(
    &mut self,
    stream: &mut S,
  ) -> Result<Frame<'a>, WebSocketError>
  where
    S: AsyncRead + Unpin,
  {
    macro_rules! eof {
      ($n:expr) => {{
        if $n == 0 {
          return Err(WebSocketError::UnexpectedEOF);
        }
      }};
    }

    // Read the first two bytes
    while self.buffer.remaining() < 2 {
      eof!(read_buf(stream, &mut self.buffer).await?);
    }

    let fin = self.buffer[0] & 0b10000000 != 0;
    let rsv1 = self.buffer[0] & 0b01000000 != 0;
    let rsv2 = self.buffer[0] & 0b00100000 != 0;
    let rsv3 = self.buffer[0] & 0b00010000 != 0;

    if rsv1 || rsv2 || rsv3 {
      return Err(WebSocketError::ReservedBitsNotZero);
    }

    let opcode = frame::OpCode::try_from(self.buffer[0] & 0b00001111)?;
    let masked = self.buffer[1] & 0b10000000 != 0;

    let length_code = self.buffer[1] & 0x7F;
    let extra = match length_code {
      126 => 2,
      127 => 8,
      _ => 0,
    };

    let header_len = 2 + extra + masked as usize * 4;
    while self.buffer.remaining() < header_len {
      eof!(read_buf(stream, &mut self.buffer).await?);
    }

    let payload_len: usize = match extra {
      0 => usize::from(length_code),
      2 => u16::from_be_bytes(self.buffer[2..4].try_into().unwrap()) as usize,
      #[cfg(target_pointer_width = "64")]
      8 => u64::from_be_bytes(self.buffer[2..10].try_into().unwrap()) as usize,
      // On 32bit systems, usize is only 4bytes wide so we must check for usize overflowing
      #[cfg(any(target_pointer_width = "16", target_pointer_width = "32"))]
      8 => match usize::try_from(u64::from_be_bytes(
        self.buffer[2..10].try_into().unwrap(),
      )) {
        Ok(length) => length,
        Err(_) => return Err(WebSocketError::FrameTooLarge),
      },
      _ => unreachable!(),
    };

    let mask = if masked {
      let mask_start = 2 + extra;
      Some(self.buffer[mask_start..mask_start + 4].try_into().unwrap())
    } else {
      None
    };

    // RFC 6455 requires every frame sent by a client to be masked. Frames sent
    // by a server are never required to be masked, so keep the client role's
    // existing permissive behavior unchanged.
    if self.role == Role::Server && !masked {
      return Err(WebSocketError::UnmaskedFrame);
    }

    if frame::is_control(opcode) && !fin {
      return Err(WebSocketError::ControlFrameFragmented);
    }

    if frame::is_control(opcode) && payload_len > 125 {
      return Err(if opcode == OpCode::Ping {
        WebSocketError::PingFrameTooLarge
      } else {
        WebSocketError::ControlFrameTooLarge
      });
    }

    if payload_len >= self.max_message_size {
      return Err(WebSocketError::FrameTooLarge);
    }

    self.buffer.reserve(payload_len + MAX_HEADER_SIZE);
    while payload_len > self.buffer.remaining() - header_len {
      eof!(read_buf(stream, &mut self.buffer).await?);
    }

    self.buffer.advance(header_len);
    let payload = self.buffer.split_to(payload_len);
    let frame = Frame::new(fin, opcode, mask, Payload::Bytes(payload));
    Ok(frame)
  }
}

impl WriteHalf {
  pub fn after_handshake(role: Role) -> Self {
    Self {
      role,
      closed: false,
      auto_apply_mask: true,
      vectored: true,
      writev_threshold: 1024,
      write_buffer: Vec::with_capacity(2),
    }
  }

  /// Writes a frame to the provided stream.
  pub async fn write_frame<'a, S>(
    &'a mut self,
    stream: &mut S,
    mut frame: Frame<'a>,
  ) -> Result<(), WebSocketError>
  where
    S: AsyncWrite + Unpin,
  {
    if self.role == Role::Client && self.auto_apply_mask {
      frame.mask()?;
    }

    if self.closed {
      if frame.opcode == OpCode::Close {
        return Ok(()); // Already sent close, this is a no-op
      }
      return Err(WebSocketError::ConnectionClosed);
    }
    let is_close = frame.opcode == OpCode::Close;
    if is_close {
      self.closed = true;
    }

    if self.vectored && frame.payload.len() > self.writev_threshold {
      frame.writev(stream).await?;
    } else {
      let text = frame.write(&mut self.write_buffer);
      stream.write_all(text).await?;
    }

    Ok(())
  }
}
