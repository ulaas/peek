//! Export a recording to MP4 from the command line:
//!
//! `cargo run -p peek-record --features mp4 --example export -- in.peekrec out.mp4 [--check frame.bmp]`
//!
//! With `--check`, the MP4 is decoded again and the middle frame saved as BMP.

use peek_record::export::{Mp4Options, export_mp4};
use std::io::{BufReader, Write};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() >= 2, "usage: export <in.peekrec> <out.mp4> [--check frame.bmp]");
    let (input, output) = (std::path::Path::new(&args[0]), std::path::Path::new(&args[1]));
    let started = std::time::Instant::now();
    let mut last = -1i32;
    let s = export_mp4(input, output, Mp4Options::default(), |p| {
        let pct = (p * 100.0) as i32;
        if pct / 10 != last / 10 {
            eprint!("{pct}% ");
            last = pct;
        }
        true
    })?;
    eprintln!();
    println!(
        "{}x{}, {} frames, {:.1} s of video, {} KB, encoded in {:.2} s",
        s.width,
        s.height,
        s.frames,
        s.duration_ms as f64 / 1000.0,
        s.bytes / 1024,
        started.elapsed().as_secs_f64()
    );

    if let Some(i) = args.iter().position(|a| a == "--check") {
        let bmp = args.get(i + 1).map(String::as_str).unwrap_or("frame.bmp");
        check(output, bmp)?;
    }
    Ok(())
}

/// Decode every sample with OpenH264 and write the middle frame as a BMP.
fn check(mp4_path: &std::path::Path, bmp: &str) -> anyhow::Result<()> {
    let f = std::fs::File::open(mp4_path)?;
    let size = f.metadata()?.len();
    let mut mp4 = mp4::Mp4Reader::read_header(BufReader::new(f), size)?;
    let (&track_id, track) = mp4.tracks().iter().next().ok_or_else(|| anyhow::anyhow!("no track"))?;
    let count = track.sample_count();
    let (sps, pps) = (track.sequence_parameter_set()?.to_vec(), track.picture_parameter_set()?.to_vec());
    let mut dec = openh264::decoder::Decoder::new()?;
    let annexb = |nal: &[u8]| [&[0u8, 0, 0, 1][..], nal].concat();
    dec.decode(&[annexb(&sps), annexb(&pps)].concat())?;
    let (mut decoded, mut saved) = (0, false);
    for n in 1..=count {
        let Some(sample) = mp4.read_sample(track_id, n)? else { continue };
        // AVCC (length-prefixed) back to Annex B for the decoder.
        let mut data = &sample.bytes[..];
        let mut stream = Vec::new();
        while data.len() >= 4 {
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            stream.extend_from_slice(&annexb(&data[4..4 + len]));
            data = &data[4 + len..];
        }
        if let Some(yuv) = dec.decode(&stream)? {
            decoded += 1;
            if !saved && n >= count / 2 {
                use openh264::formats::YUVSource;
                let (w, h) = yuv.dimensions();
                let mut rgb = vec![0u8; w * h * 3];
                yuv.write_rgb8(&mut rgb);
                write_bmp(bmp, w, h, &rgb)?;
                saved = true;
            }
        }
    }
    println!("decoded {decoded} of {count} frames; middle frame -> {bmp}");
    anyhow::ensure!(decoded == count, "some frames did not decode");
    Ok(())
}

fn write_bmp(path: &str, w: usize, h: usize, rgb: &[u8]) -> std::io::Result<()> {
    let row = (w * 3).div_ceil(4) * 4;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let size = 54 + row * h;
    f.write_all(b"BM")?;
    for v in [size as u32, 0, 54, 40, w as u32, h as u32] {
        f.write_all(&v.to_le_bytes())?;
    }
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    for v in [0u32, (row * h) as u32, 2835, 2835, 0, 0] {
        f.write_all(&v.to_le_bytes())?;
    }
    for y in (0..h).rev() {
        let mut line = vec![0u8; row];
        for x in 0..w {
            let p = &rgb[(y * w + x) * 3..][..3];
            line[x * 3..x * 3 + 3].copy_from_slice(&[p[2], p[1], p[0]]);
        }
        f.write_all(&line)?;
    }
    Ok(())
}
