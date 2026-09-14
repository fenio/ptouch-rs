// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>
// SPDX-FileCopyrightText: Dominic Radermacher and the ptouch-print contributors
//
// Portions derived from ptouch-print, licensed GPL-3.0-or-later:
// https://git.familie-radermacher.ch/linux/ptouch-print.git

//! Model capabilities independent of connection identifiers.

use crate::device::{DeviceFlags, DeviceInfo};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelProfile {
    pub(crate) name: &'static str,
    pub(crate) max_px: u16,
    pub(crate) dpi: u16,
    pub(crate) flags: DeviceFlags,
}

impl From<&DeviceInfo> for ModelProfile {
    fn from(info: &DeviceInfo) -> Self {
        Self {
            name: info.name,
            max_px: info.max_px,
            dpi: info.dpi,
            flags: info.flags,
        }
    }
}
