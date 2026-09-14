// SPDX-License-Identifier: GPL-3.0-or-later
//! PT-P300BT command dialect, verified by the native Bluetooth probe.
use crate::{
    error::{PtouchError, Result},
    protocol,
    status::PrinterStatus,
};

pub(crate) fn cmd_init() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes.extend([0x1b, 0x40, 0x1b, 0x69, 0x61, 1]);
    bytes
}

pub(crate) fn build_print_job(
    lines: &[Vec<u8>],
    status: &PrinterStatus,
    chain_print: bool,
) -> Result<Vec<Vec<u8>>> {
    if status.has_error() || status.status_type == 2 || status.status_type == 4 {
        return Err(PtouchError::StatusError(status.error_description()));
    }
    if status.status_type != 0
        || status.phase_type != 0
        || status.phase_number_hi != 0
        || status.phase_number_lo != 0
    {
        return Err(PtouchError::StatusError("PT-P300BT is not idle".into()));
    }
    if status.model_code != 0x72 {
        return Err(PtouchError::StatusError("Expected PT-P300BT status".into()));
    }
    if status.media_width != 12 {
        return Err(PtouchError::UnknownTapeWidth(status.media_width));
    }
    if lines.is_empty() || lines.iter().any(|line| line.len() != 16) {
        return Err(PtouchError::SendFailed(
            "PT-P300BT requires nonempty raster data with 16 bytes per line".into(),
        ));
    }
    // Reject marks outside the physically verified 64-dot printable area.
    if lines
        .iter()
        .any(|line| line[..4].iter().chain(&line[12..]).any(|&b| b != 0))
    {
        return Err(PtouchError::UnsupportedRaster(
            "PT-P300BT requires white padding outside the middle 64 dots".into(),
        ));
    }
    let count = u32::try_from(lines.len())
        .map_err(|_| PtouchError::SendFailed("Too many raster lines".into()))?;
    let mut info = vec![
        0x1b,
        0x69,
        0x7a,
        0xc4,
        status.media_type,
        status.media_width,
        status.media_length,
    ];
    info.extend(count.to_le_bytes());
    info.extend([0, 0]);
    let mut job = vec![
        cmd_init(),
        info,
        vec![0x1b, 0x69, 0x4b, if chain_print { 0 } else { 8 }],
        vec![0x1b, 0x69, 0x4d, 0],
        protocol::cmd_page_flags(0),
        vec![0x4d, 0],
    ];
    for line in lines {
        job.push(if protocol::rasterline_is_blank(line) {
            protocol::cmd_line_feed()
        } else {
            protocol::cmd_send_raster(line)
        });
    }
    job.push(protocol::cmd_finalize(
        chain_print,
        crate::device::DeviceFlags::NONE,
    ));
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> PrinterStatus {
        let mut bytes = [0; 32];
        bytes[..5].copy_from_slice(&[0x80, 0x20, 0x42, 0x30, 0x72]);
        bytes[10] = 12;
        bytes[11] = 1;
        PrinterStatus::from_bytes(&bytes).unwrap()
    }
    #[test]
    fn job_matches_verified_probe_commands() {
        let mut line = vec![0; 16];
        line[5] = 0x40;
        let job = build_print_job(&[vec![0; 16], line], &ready(), false).unwrap();
        let mut init = vec![0; 64];
        init.extend([0x1b, 0x40, 0x1b, 0x69, 0x61, 1]);
        assert_eq!(job[0], init);
        assert_eq!(job[1], [0x1b, 0x69, 0x7a, 0xc4, 1, 12, 0, 2, 0, 0, 0, 0, 0]);
        assert_eq!(job[2], [0x1b, 0x69, 0x4b, 8]);
        assert_eq!(job[3], [0x1b, 0x69, 0x4d, 0]);
        assert_eq!(job[4], [0x1b, 0x69, 0x64, 0, 0]);
        assert_eq!(job[5], [0x4d, 0]);
        assert_eq!(job[6], [0x5a]);
        assert_eq!(&job[7][..3], [0x47, 16, 0]);
        assert_eq!(job[7][8], 0x40);
        assert_eq!(job[8], [0x1a]);
    }
    #[test]
    fn invalid_jobs_are_rejected_before_sending() {
        assert!(build_print_job(&[], &ready(), false).is_err());
        assert!(build_print_job(&[vec![0; 8]], &ready(), false).is_err());
        assert!(build_print_job(&[vec![1; 16]], &ready(), false).is_err());
        let mut status = ready();
        status.media_width = 9;
        assert!(matches!(
            build_print_job(&[vec![0; 16]], &status, false),
            Err(PtouchError::UnknownTapeWidth(9))
        ));
        status = ready();
        status.phase_type = 1;
        assert!(build_print_job(&[vec![0; 16]], &status, false).is_err());
        status = ready();
        status.error_info_1 = 8;
        assert!(build_print_job(&[vec![0; 16]], &status, false).is_err());
    }
}
