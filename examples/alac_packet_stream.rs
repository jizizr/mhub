//! Offline transport fixture generator used by scripts/verify_alac_browser.mjs.
//! Accepts a local ALAC MP4 and writes the exact browser transport.
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let input = args
        .next()
        .ok_or("usage: alac_packet_stream input.m4a output.alacstream")?;
    let output = args.next().ok_or("missing output path")?;
    let source = fs::read(input)?;
    let packets = music_api::music::alac_transport_from_progressive(&source)?;
    fs::write(output, packets)?;
    Ok(())
}
