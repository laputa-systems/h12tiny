//! Adaptors from AsyncRead/AsyncWrite to Stream/Sink
//!
//! Raw I/O objects work with byte sequences, but higher-level code
//! usually wants to batch these into meaningful chunks, called
//! "frames".
//!
//! This module contains adapters to go from streams of bytes,
//! [`AsyncRead`] and [`AsyncWrite`], to framed streams implementing
//! [`Sink`] and [`Stream`].  Framed streams are also known as
//! transports.
//!
//! [`AsyncRead`]: futures_io::AsyncRead
//! [`AsyncWrite`]: futures_io::AsyncWrite
//! [`Stream`]: tokio::stream::Stream
//! [`Sink`]: futures_sink::Sink

use bytes::Buf;
use futures_core::ready as futures_ready;
use futures_io::AsyncWrite;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) fn poll_write_buf<T, B>(
    mut io: Pin<&mut T>,
    cx: &mut Context<'_>,
    buf: &mut B,
) -> Poll<io::Result<usize>>
where
    T: AsyncWrite + ?Sized,
    B: Buf,
{
    if !buf.has_remaining() {
        return Poll::Ready(Ok(0));
    }

    let n = futures_ready!(io.as_mut().poll_write(cx, buf.chunk()))?;
    buf.advance(n);
    Poll::Ready(Ok(n))
}

mod bytes_codec;
pub use self::bytes_codec::BytesCodec;

mod decoder;
pub use self::decoder::Decoder;

mod encoder;
pub use self::encoder::Encoder;

mod framed_impl;
#[allow(unused_imports)]
pub(crate) use self::framed_impl::{FramedImpl, RWFrames, ReadFrame, WriteFrame};

mod framed;
pub use self::framed::{Framed, FramedParts};

mod framed_read;
pub use self::framed_read::FramedRead;

mod framed_write;
pub use self::framed_write::FramedWrite;

pub mod length_delimited;
pub use self::length_delimited::{LengthDelimitedCodec, LengthDelimitedCodecError};

mod lines_codec;
pub use self::lines_codec::{LinesCodec, LinesCodecError};
