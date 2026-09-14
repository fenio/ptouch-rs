// SPDX-License-Identifier: GPL-3.0-or-later
//! Small native Bluetooth entry point, using the production session/backend.
#[cfg(any(test, all(feature = "bluetooth", target_os = "macos")))]
// Fixed 5x7 glyphs make the feasibility label independent of GUI/font dependencies.
fn raster() -> Vec<[u8; 16]> {
    let glyphs = [
        [30, 17, 17, 30, 20, 18, 17],
        [17, 17, 17, 17, 17, 17, 14],
        [15, 16, 16, 14, 1, 1, 30],
        [31, 4, 4, 4, 4, 4, 4],
    ];
    let mut lines = vec![[0u8; 16]; 156];
    for (g, rows) in glyphs.iter().enumerate() {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    for dx in 0..6 {
                        for dy in 0..6 {
                            let x = 6 + g * 36 + col * 6 + dx;
                            let y = 11 + row * 6 + dy;
                            // 64 printable dots centered in the 128-dot transfer line.
                            // Transfer dots run bottom-to-top, matching the existing renderer.
                            let bit = 32 + (63 - y);
                            lines[x][15 - bit / 8] |= 1 << (bit % 8);
                        }
                    }
                }
            }
        }
    }
    lines
}

#[cfg(all(feature = "bluetooth", target_os = "macos"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || !matches!(args[0].as_str(), "status" | "print") {
        return Err("Usage: bluetooth status|print ADDRESS [REPEAT]".into());
    }
    let repeat = args
        .get(2)
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(1);
    if repeat == 0 || (args[0] == "print" && repeat != 1) {
        return Err("Use positive repetitions; print is limited to one label".into());
    }
    for i in 1..=repeat {
        let mut printer = ptouch_core::BluetoothDevice::open(&args[1])?;
        printer.init()?;
        println!(
            "session {i}/{repeat}: {} raster={} dots printable={:?} status={:?}",
            printer.model_name(),
            printer.raster_width_px(),
            printer.tape_width_px(),
            printer.status()
        );
        if args[0] == "print" {
            let lines: Vec<Vec<u8>> = raster().into_iter().map(|line| line.to_vec()).collect();
            printer.print_raster(&lines)?;
            println!("Printer confirmed completion: {:?}", printer.status());
        }
        printer.close()?;
    }
    Ok(())
}
#[cfg(all(feature = "bluetooth", target_os = "macos"))]
fn main() {
    if let Err(error) = objc2::rc::autoreleasepool(|_| run()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(not(all(feature = "bluetooth", target_os = "macos")))]
fn main() {
    eprintln!("Enable the bluetooth feature and run on macOS");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asymmetric_glyph_survives_printer_dot_order() {
        let raster = raster();
        let expected = [
            "11110", "10001", "10001", "11110", "10100", "10010", "10001",
        ];
        for (row, pattern) in expected.iter().enumerate() {
            for (col, pixel) in pattern.bytes().enumerate() {
                let wire_dot = 127 - (32 + 11 + row * 6);
                let line = &raster[6 + col * 6];
                let black = line[15 - wire_dot / 8] & (1 << (wire_dot % 8)) != 0;
                assert_eq!(black, pixel == b'1', "row {row} column {col}");
            }
        }
    }
}
