//! Buffered big-endian reader with byte accounting.

use peek_core::SessionStats;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

pub struct Reader<R> {
    inner: BufReader<R>,
    stats: Option<Arc<SessionStats>>,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    pub fn new(r: R, stats: Option<Arc<SessionStats>>) -> Self {
        Self { inner: BufReader::with_capacity(256 * 1024, r), stats }
    }

    #[inline]
    fn count(&self, n: usize) {
        if let Some(s) = &self.stats {
            s.add_in(n);
        }
    }

    pub async fn exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buf).await?;
        self.count(buf.len());
        Ok(())
    }

    pub async fn u8(&mut self) -> io::Result<u8> {
        let v = self.inner.read_u8().await?;
        self.count(1);
        Ok(v)
    }

    pub async fn u16(&mut self) -> io::Result<u16> {
        let v = self.inner.read_u16().await?;
        self.count(2);
        Ok(v)
    }

    pub async fn u32(&mut self) -> io::Result<u32> {
        let v = self.inner.read_u32().await?;
        self.count(4);
        Ok(v)
    }

    pub async fn i32(&mut self) -> io::Result<i32> {
        Ok(self.u32().await? as i32)
    }

    /// Read `n` bytes into `buf` (cleared first).
    pub async fn read_into(&mut self, n: usize, buf: &mut Vec<u8>) -> io::Result<()> {
        buf.clear();
        buf.resize(n, 0);
        self.exact(buf).await
    }

    pub async fn vec(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.read_into(n, &mut v).await?;
        Ok(v)
    }

    pub async fn skip(&mut self, mut n: usize) -> io::Result<()> {
        let mut scratch = [0u8; 4096];
        while n > 0 {
            let k = n.min(scratch.len());
            self.exact(&mut scratch[..k]).await?;
            n -= k;
        }
        Ok(())
    }

    /// Tight "compact length": 1–3 bytes, 7 bits each, low bits first.
    pub async fn compact_len(&mut self) -> io::Result<usize> {
        let mut len = 0usize;
        for i in 0..3 {
            let b = self.u8().await?;
            if i == 2 {
                len |= (b as usize) << 14;
                break;
            }
            len |= ((b & 0x7f) as usize) << (7 * i);
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(len)
    }
}

/// Builder for client messages.
#[derive(Default)]
pub struct Msg(pub Vec<u8>);

impl Msg {
    pub fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(self, v: i32) -> Self {
        self.u32(v as u32)
    }
    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }
    pub fn pad(mut self, n: usize) -> Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
}
