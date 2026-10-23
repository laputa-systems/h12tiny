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

use std::future::Future;

use crate::error::WebSocketError;
use crate::frame::Frame;
use crate::OpCode;
use crate::ReadHalf;
use crate::WebSocketRead;
use futures_lite::io::AsyncRead;

pub enum Fragment {
  Text(Vec<u8>, Vec<u8>, usize),
  Binary(Vec<u8>),
}

impl Fragment {
  fn take_buffer(self) -> Vec<u8> {
    match self {
      Fragment::Text(_, buffer, _) => buffer,
      Fragment::Binary(buffer) => buffer,
    }
  }
}

/// Collects fragmented messages over a WebSocket connection and returns the completed message once all fragments have been received.
///
/// This is useful for applications that do not want to deal with fragmented messages and the default behavior of tungstenite.
/// The payload is buffered in memory until the final fragment is received
/// so use this when streaming messages is not an option.
pub struct FragmentCollectorRead<S> {
  stream: S,
  read_half: ReadHalf,
  fragments: Fragments,
}

impl<'f, S> FragmentCollectorRead<S> {
  /// Creates a new `FragmentCollector` with the provided `WebSocket`.
  pub fn new(ws: WebSocketRead<S>) -> FragmentCollectorRead<S>
  where
    S: AsyncRead + Unpin,
  {
    let (stream, read_half) = ws.into_parts_internal();
    FragmentCollectorRead {
      stream,
      read_half,
      fragments: Fragments::new(),
    }
  }

  /// Reads a WebSocket frame, collecting fragmented messages until the final frame is received and returns the completed message.
  ///
  /// Text frames payload is guaranteed to be valid UTF-8.
  ///
  /// # Arguments
  ///
  /// * `send_fn`: Closure must ensure frames are sent by write side of split WebSocket to correctly implement auto-close and auto-pong.
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
      let Some(frame) = res? else {
        continue;
      };
      if let Some(frame) = self
        .fragments
        .accumulate(frame, self.read_half.max_message_size)?
      {
        return Ok(frame);
      }
    }
  }
}

/// Accumulates potentially fragmented [`Frame`]s to defragment the incoming WebSocket stream.
struct Fragments {
  fragments: Option<Fragment>,
  opcode: OpCode,
}

impl Fragments {
  pub fn new() -> Self {
    Self {
      fragments: None,
      opcode: OpCode::Close,
    }
  }

  pub fn accumulate<'f>(
    &mut self,
    frame: Frame<'f>,
    max_message_size: usize,
  ) -> Result<Option<Frame<'f>>, WebSocketError> {
    match frame.opcode {
      OpCode::Text | OpCode::Binary => {
        if frame.fin {
          if self.fragments.is_some() {
            return Err(WebSocketError::InvalidFragment);
          }
          if frame.opcode == OpCode::Text
            && simdutf8::compat::from_utf8(&frame.payload).is_err()
          {
            return Err(WebSocketError::InvalidUTF8);
          }
          return Ok(Some(Frame::new(true, frame.opcode, None, frame.payload)));
        } else {
          if frame.payload.len() >= max_message_size {
            return Err(WebSocketError::FrameTooLarge);
          }
          self.fragments = match frame.opcode {
            OpCode::Text => {
              let mut buf = Vec::new();
              let mut pending = Vec::new();
              let payload = &frame.payload[..];
              match simdutf8::compat::from_utf8(payload) {
                Ok(text) => {
                  buf.extend_from_slice(text.as_bytes());
                }
                Err(e) => {
                  if e.error_len().is_none() {
                    buf.extend_from_slice(&payload[..e.valid_up_to()]);
                    pending.extend_from_slice(&payload[e.valid_up_to()..]);
                  } else {
                    return Err(WebSocketError::InvalidUTF8);
                  }
                }
              }
              Some(Fragment::Text(pending, buf, frame.payload.len()))
            }
            OpCode::Binary => Some(Fragment::Binary(frame.payload.into())),
            _ => unreachable!(),
          };
          self.opcode = frame.opcode;
        }
      }
      OpCode::Continuation => match self.fragments.as_mut() {
        None => {
          return Err(WebSocketError::InvalidContinuationFrame);
        }
        Some(Fragment::Text(pending, input, message_len)) => {
          let new_message_len = message_len
            .checked_add(frame.payload.len())
            .ok_or(WebSocketError::FrameTooLarge)?;
          if new_message_len >= max_message_size {
            return Err(WebSocketError::FrameTooLarge);
          }
          *message_len = new_message_len;

          let mut combined =
            Vec::with_capacity(pending.len() + frame.payload.len());
          combined.extend_from_slice(pending);
          combined.extend_from_slice(&frame.payload);

          match simdutf8::compat::from_utf8(&combined) {
            Ok(text) => {
              input.extend_from_slice(text.as_bytes());
              pending.clear();
            }
            Err(e) => {
              input.extend_from_slice(&combined[..e.valid_up_to()]);
              if e.error_len().is_none() {
                *pending = combined[e.valid_up_to()..].to_vec();
              } else {
                return Err(WebSocketError::InvalidUTF8);
              }
            }
          }

          if frame.fin {
            if !pending.is_empty() {
              return Err(WebSocketError::InvalidUTF8);
            }
            return Ok(Some(Frame::new(
              true,
              self.opcode,
              None,
              self.fragments.take().unwrap().take_buffer().into(),
            )));
          }
        }
        Some(Fragment::Binary(data)) => {
          let message_len = data
            .len()
            .checked_add(frame.payload.len())
            .ok_or(WebSocketError::FrameTooLarge)?;
          if message_len >= max_message_size {
            return Err(WebSocketError::FrameTooLarge);
          }

          data.extend_from_slice(&frame.payload);
          if frame.fin {
            return Ok(Some(Frame::new(
              true,
              self.opcode,
              None,
              self.fragments.take().unwrap().take_buffer().into(),
            )));
          }
        }
      },
      _ => return Ok(Some(frame)),
    }

    Ok(None)
  }
}
