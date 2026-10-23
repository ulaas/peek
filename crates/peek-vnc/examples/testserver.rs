//! Minimal RFB 3.8 server for end-to-end testing of the Peek client.
//!
//! Serves an animated test pattern, encodes with ZRLE when the client
//! supports it (Raw otherwise), and draws a dot wherever the client's
//! pointer is, so input round-trips are visible.
//!
//! ```text
//! cargo run -p peek-vnc --example testserver -- [port] [password]
//! ```

use flate2::{Compress, Compression, FlushCompress};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const W: usize = 1280;
const H: usize = 720;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let port = args.get(1).map(String::as_str).unwrap_or("5999");
    let password = args.get(2).cloned();
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?;
    println!("test VNC server on 127.0.0.1:{port} (password: {})", password.as_deref().unwrap_or("none"));
    for stream in listener.incoming() {
        let stream = stream?;
        let pw = password.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, pw) {
                println!("client gone: {e}");
            }
        });
    }
    Ok(())
}

fn read_exact<const N: usize>(s: &mut TcpStream) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    s.read_exact(&mut b)?;
    Ok(b)
}

fn serve(mut s: TcpStream, password: Option<String>) -> std::io::Result<()> {
    s.set_nodelay(true)?;
    s.write_all(b"RFB 003.008\n")?;
    let _client_version: [u8; 12] = read_exact(&mut s)?;
    match &password {
        None => {
            s.write_all(&[1, 1])?;
            let _: [u8; 1] = read_exact(&mut s)?;
        }
        Some(pw) => {
            s.write_all(&[1, 2])?;
            let _: [u8; 1] = read_exact(&mut s)?;
            let challenge: [u8; 16] = *b"peek-test-chall!";
            s.write_all(&challenge)?;
            let resp: [u8; 16] = read_exact(&mut s)?;
            if resp != peek_vnc::vnc_auth_response(&challenge, pw.as_bytes()) {
                s.write_all(&1u32.to_be_bytes())?;
                let msg = b"wrong password";
                s.write_all(&(msg.len() as u32).to_be_bytes())?;
                s.write_all(msg)?;
                return Ok(());
            }
        }
    }
    s.write_all(&0u32.to_be_bytes())?; // SecurityResult OK
    let _shared: [u8; 1] = read_exact(&mut s)?;
    let mut init = Vec::new();
    init.extend_from_slice(&(W as u16).to_be_bytes());
    init.extend_from_slice(&(H as u16).to_be_bytes());
    init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0]);
    let name = b"Peek test server";
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name);
    s.write_all(&init)?;

    let start = Instant::now();
    let mut zrle = false;
    let mut z = Compress::new(Compression::fast(), true);
    let mut pointer = (W / 2, H / 2);
    let mut frames = 0u64;
    loop {
        let [ty]: [u8; 1] = read_exact(&mut s)?;
        match ty {
            0 => {
                let _: [u8; 19] = read_exact(&mut s)?;
            }
            2 => {
                let [_, a, b]: [u8; 3] = read_exact(&mut s)?;
                let n = u16::from_be_bytes([a, b]) as usize;
                let mut enc = vec![0u8; n * 4];
                s.read_exact(&mut enc)?;
                zrle = enc.as_chunks::<4>().0.iter().any(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]) == 16);
                println!("client encodings: {} (ZRLE: {zrle})", n);
            }
            3 => {
                let _: [u8; 9] = read_exact(&mut s)?;
                // Pace to ~60 fps.
                let due = Duration::from_micros(16_666 * frames);
                if let Some(wait) = due.checked_sub(start.elapsed()) {
                    std::thread::sleep(wait);
                }
                frames += 1;
                send_frame(&mut s, start.elapsed().as_secs_f32(), pointer, zrle, &mut z)?;
            }
            4 => {
                let _: [u8; 7] = read_exact(&mut s)?;
            }
            5 => {
                let [_, x0, x1, y0, y1]: [u8; 5] = read_exact(&mut s)?;
                pointer = (u16::from_be_bytes([x0, x1]) as usize, u16::from_be_bytes([y0, y1]) as usize);
            }
            6 => {
                let [_, _, _, a, b, c, d]: [u8; 7] = read_exact(&mut s)?;
                let mut text = vec![0u8; u32::from_be_bytes([a, b, c, d]) as usize];
                s.read_exact(&mut text)?;
                println!("clipboard from client: {:?}", String::from_utf8_lossy(&text));
            }
            other => {
                println!("unexpected client message {other}");
                return Ok(());
            }
        }
    }
}

/// One horizontal band of the picture changes per frame (plus the pointer
/// area), which mimics a typical desktop: partial, localized updates.
fn send_frame(s: &mut TcpStream, t: f32, pointer: (usize, usize), zrle: bool, z: &mut Compress) -> std::io::Result<()> {
    let band_h = 90;
    let band = ((t * 2.0) as usize) % (H / band_h);
    let rects = [
        (0, band * band_h, W, band_h),
        (pointer.0.saturating_sub(20).min(W - 40), pointer.1.saturating_sub(20).min(H - 40), 40, 40),
    ];
    let mut msg = vec![0u8, 0];
    msg.extend_from_slice(&(rects.len() as u16).to_be_bytes());
    for &(x, y, w, h) in &rects {
        msg.extend_from_slice(&(x as u16).to_be_bytes());
        msg.extend_from_slice(&(y as u16).to_be_bytes());
        msg.extend_from_slice(&(w as u16).to_be_bytes());
        msg.extend_from_slice(&(h as u16).to_be_bytes());
        let px = |px: usize, py: usize| -> [u8; 3] {
            let (dx, dy) = (px as f32 - pointer.0 as f32, py as f32 - pointer.1 as f32);
            if dx * dx + dy * dy < 64.0 {
                return [255, 80, 80];
            }
            let r = ((px as f32 / W as f32) * 255.0) as u8;
            let g = ((py as f32 / H as f32) * 255.0) as u8;
            let b = (((px + py) as f32 * 0.02 + t * 3.0).sin() * 127.0 + 128.0) as u8;
            if (px / 80 + py / 80).is_multiple_of(2) { [r, g, b] } else { [r / 2, g / 2, b / 2] }
        };
        if zrle {
            msg.extend_from_slice(&16i32.to_be_bytes());
            let mut raw = Vec::with_capacity(w * h * 3 + 64);
            for ty in (0..h).step_by(64) {
                for tx in (0..w).step_by(64) {
                    raw.push(0); // raw CPIXEL tile
                    for yy in ty..(ty + 64).min(h) {
                        for xx in tx..(tx + 64).min(w) {
                            raw.extend_from_slice(&px(x + xx, y + yy));
                        }
                    }
                }
            }
            let mut out = Vec::with_capacity(raw.len() + 1024);
            z.compress_vec(&raw, &mut out, FlushCompress::Sync).map_err(std::io::Error::other)?;
            while out.len() == out.capacity() {
                out.reserve(64 * 1024);
                z.compress_vec(&[], &mut out, FlushCompress::Sync).map_err(std::io::Error::other)?;
            }
            msg.extend_from_slice(&(out.len() as u32).to_be_bytes());
            msg.extend_from_slice(&out);
        } else {
            msg.extend_from_slice(&0i32.to_be_bytes());
            for yy in 0..h {
                for xx in 0..w {
                    let [r, g, b] = px(x + xx, y + yy);
                    msg.extend_from_slice(&[r, g, b, 0]);
                }
            }
        }
    }
    s.write_all(&msg)
}
