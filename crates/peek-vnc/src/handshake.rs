//! RFB protocol version negotiation, security and initialisation (RFC 6143 §7.1–7.3).

use crate::io::Reader;
use anyhow::{Context, bail};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub const SEC_NONE: u8 = 1;
pub const SEC_VNC: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Version {
    V3_3,
    V3_7,
    V3_8,
}

impl Version {
    fn banner(self) -> &'static [u8; 12] {
        match self {
            Version::V3_3 => b"RFB 003.003\n",
            Version::V3_7 => b"RFB 003.007\n",
            Version::V3_8 => b"RFB 003.008\n",
        }
    }
}

pub fn parse_version(b: &[u8; 12]) -> anyhow::Result<Version> {
    let s = std::str::from_utf8(b).context("server banner is not text")?;
    if !s.starts_with("RFB ") || s.as_bytes()[7] != b'.' {
        bail!("not a VNC server (banner {s:?})");
    }
    let major: u32 = s[4..7].parse().context("bad major version")?;
    let minor: u32 = s[8..11].parse().context("bad minor version")?;
    Ok(match (major, minor) {
        (3, 0..=6) => Version::V3_3,
        (3, 7) => Version::V3_7,
        // 3.8, Apple's 3.889, RealVNC 4.x/5.x all speak 3.8 to us.
        _ if major >= 3 => Version::V3_8,
        _ => bail!("unsupported RFB version {major}.{minor}"),
    })
}

#[derive(Clone, Debug)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
}

/// VNC authentication: DES-encrypt the 16-byte challenge with the password
/// (max 8 bytes, each byte's bits mirrored) as key.
pub fn vnc_auth_response(challenge: &[u8; 16], password: &[u8]) -> [u8; 16] {
    use des::Des;
    use des::cipher::{BlockCipherEncrypt, KeyInit};

    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password) {
        *k = b.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("8-byte key");
    let mut out = *challenge;
    for chunk in out.as_chunks_mut::<8>().0 {
        let mut block = des::cipher::Block::<Des>::try_from(&chunk[..]).expect("8-byte block");
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    out
}

async fn read_reason<R: AsyncRead + Unpin>(r: &mut Reader<R>) -> String {
    match r.u32().await {
        Ok(n) if n < 64 * 1024 => match r.vec(n as usize).await {
            Ok(v) => String::from_utf8_lossy(&v).into_owned(),
            Err(_) => "connection closed".into(),
        },
        _ => "connection closed".into(),
    }
}

pub async fn handshake<R, W>(
    r: &mut Reader<R>,
    w: &mut W,
    password: Option<&str>,
    shared: bool,
) -> anyhow::Result<ServerInit>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut banner = [0u8; 12];
    r.exact(&mut banner)
        .await
        .context("reading server version")?;
    let version = parse_version(&banner)?;
    w.write_all(version.banner()).await?;

    // Security type selection.
    let sec = if version == Version::V3_3 {
        let t = r.u32().await?;
        if t == 0 {
            bail!("server refused connection: {}", read_reason(r).await);
        }
        u8::try_from(t).context("bad security type")?
    } else {
        let n = r.u8().await?;
        if n == 0 {
            bail!("server refused connection: {}", read_reason(r).await);
        }
        let types = r.vec(n as usize).await?;
        let pick = if types.contains(&SEC_NONE) && password.is_none_or(str::is_empty) {
            SEC_NONE
        } else if types.contains(&SEC_VNC) {
            SEC_VNC
        } else if types.contains(&SEC_NONE) {
            SEC_NONE
        } else {
            bail!(
                "server requires an authentication method Peek does not support yet (offered: {types:?})"
            );
        };
        w.write_all(&[pick]).await?;
        pick
    };

    match sec {
        SEC_NONE => {}
        SEC_VNC => {
            let Some(pw) = password.filter(|p| !p.is_empty()) else {
                bail!(peek_core::AuthError::PasswordRequired);
            };
            let mut challenge = [0u8; 16];
            r.exact(&mut challenge).await?;
            w.write_all(&vnc_auth_response(&challenge, pw.as_bytes()))
                .await?;
        }
        other => bail!("unsupported security type {other}"),
    }

    // SecurityResult: always in 3.8, only after auth before that.
    if version == Version::V3_8 || sec != SEC_NONE {
        let result = r.u32().await?;
        if result != 0 {
            let reason = if version == Version::V3_8 {
                read_reason(r).await
            } else {
                String::new()
            };
            bail!(peek_core::AuthError::Rejected(reason));
        }
    }

    // ClientInit / ServerInit
    w.write_all(&[shared as u8]).await?;
    let width = r.u16().await?;
    let height = r.u16().await?;
    r.skip(16).await?; // server pixel format; we replace it
    let name_len = r.u32().await? as usize;
    let name = String::from_utf8_lossy(&r.vec(name_len.min(1 << 16)).await?).into_owned();
    Ok(ServerInit {
        width,
        height,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version(b"RFB 003.008\n").unwrap(), Version::V3_8);
        assert_eq!(parse_version(b"RFB 003.889\n").unwrap(), Version::V3_8);
        assert_eq!(parse_version(b"RFB 003.003\n").unwrap(), Version::V3_3);
        assert_eq!(parse_version(b"RFB 003.007\n").unwrap(), Version::V3_7);
        assert!(parse_version(b"SSH-2.0-Open").is_err());
    }

    #[test]
    fn vnc_auth_des_vector() {
        // FIPS test vector: key 133457799BBCDFF1, P 0123456789ABCDEF -> C 85E813540F0AB405.
        // VNC mirrors each key byte, so pass the mirrored key as "password".
        let key: Vec<u8> = [0x13u8, 0x34, 0x57, 0x79, 0x9B, 0xBC, 0xDF, 0xF1]
            .iter()
            .map(|b| b.reverse_bits())
            .collect();
        let mut challenge = [0u8; 16];
        challenge[..8].copy_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        challenge[8..].copy_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        let resp = vnc_auth_response(&challenge, &key);
        assert_eq!(
            &resp[..8],
            &[0x85, 0xE8, 0x13, 0x54, 0x0F, 0x0A, 0xB4, 0x05]
        );
        assert_eq!(&resp[..8], &resp[8..]);
    }

    #[tokio::test]
    async fn handshake_none_auth_3_8() {
        let (client, mut server) = tokio::io::duplex(4096);
        let srv = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            server.write_all(b"RFB 003.008\n").await.unwrap();
            let mut v = [0u8; 12];
            server.read_exact(&mut v).await.unwrap();
            assert_eq!(&v, b"RFB 003.008\n");
            server.write_all(&[1, SEC_NONE]).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), SEC_NONE);
            server.write_all(&0u32.to_be_bytes()).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 1); // shared
            let mut init = vec![];
            init.extend_from_slice(&800u16.to_be_bytes());
            init.extend_from_slice(&600u16.to_be_bytes());
            init.extend_from_slice(&[0; 16]);
            init.extend_from_slice(&4u32.to_be_bytes());
            init.extend_from_slice(b"desk");
            server.write_all(&init).await.unwrap();
        });
        let (rh, mut wh) = tokio::io::split(client);
        let mut r = Reader::new(rh, None);
        let init = handshake(&mut r, &mut wh, None, true).await.unwrap();
        assert_eq!(
            (init.width, init.height, init.name.as_str()),
            (800, 600, "desk")
        );
        srv.await.unwrap();
    }
}
