// SPDX-License-Identifier: GPL-3.0-or-later
//! Native macOS Bluetooth support for the PT-P300BT.
//!
//! Enable the `bluetooth` Cargo feature. Pair the printer in macOS settings,
//! grant Bluetooth access to the calling application, and open it by address.
//! Native objects must stay on the main thread; methods service its run loop.
//! Do not call blocking session methods from an egui frame callback. GUI service
//! integration is a separate step. This connection assumes exclusive ownership
//! of the printer: close also disconnects its Bluetooth baseband connection.
mod macos;

use crate::{
    error::{PtouchError, Result},
    model::ModelProfile,
    protocol::PrintQuality,
    session::PrinterSession,
    status::PrinterStatus,
};
use macos::NativeTransport;

/// A main-thread native Bluetooth connection to a PT-P300BT.
///
/// Supports the physically verified 12mm tape profile. It is neither `Send`
/// nor `Sync`. Raster buffers are 16 bytes wide; only the middle 64 dots may
/// be marked. The existing renderer's bottom-to-top dot order is required.
/// USB constructors and discovery are unaffected by Bluetooth support.
///
/// Native handles cannot be moved across threads:
/// ```compile_fail
/// fn requires_send<T: Send>() {}
/// requires_send::<ptouch_core::BluetoothDevice>();
/// ```
/// Nor can they be shared across threads:
/// ```compile_fail
/// fn requires_sync<T: Sync>() {}
/// requires_sync::<ptouch_core::BluetoothDevice>();
/// ```
pub struct BluetoothDevice {
    session: PrinterSession<NativeTransport>,
}

impl BluetoothDevice {
    /// Open an already-paired printer by its colon-separated Bluetooth address.
    ///
    /// Must be called on the main thread. Call [`Self::init`] before printing.
    pub fn open(address: &str) -> Result<Self> {
        Ok(Self {
            session: PrinterSession::new(NativeTransport::open(address)?, ModelProfile::P300BT),
        })
    }
    /// Model name, independent of USB identifiers.
    pub fn model_name(&self) -> &'static str {
        ModelProfile::P300BT.name
    }
    /// Resolution in dots per inch.
    pub fn dpi(&self) -> u16 {
        ModelProfile::P300BT.dpi
    }
    /// Raster transfer width, distinct from the printable tape width.
    pub fn raster_width_px(&self) -> u16 {
        ModelProfile::P300BT.raster_width_px
    }
    /// Printable dot count for the current tape, or None for an unverified width.
    pub fn tape_width_px(&self) -> Option<u16> {
        self.session.tape_width_px()
    }
    /// Most recently received status.
    pub fn status(&self) -> Option<&PrinterStatus> {
        self.session.status()
    }
    /// Whether initialization succeeded.
    pub fn is_initialized(&self) -> bool {
        self.session.is_initialized()
    }
    /// Initialize raster mode and query status, without inventing USB IDs.
    pub fn init(&mut self) -> Result<()> {
        self.session.init()
    }
    /// Query status without resetting the printer.
    pub fn query_status(&mut self) -> Result<&PrinterStatus> {
        self.session.query_status()
    }
    /// Print one label and wait for the printer's completion notification.
    ///
    /// Rejects unverified tape widths and marks outside the printable area.
    /// Phase changes do not indicate completion; errors and timeouts fail the
    /// operation. A failed job is never automatically resubmitted.
    /// The PT-P300BT has a manual cutter.
    pub fn print_raster(&mut self, lines: &[Vec<u8>]) -> Result<()> {
        if !self.session.is_initialized() {
            return Err(PtouchError::NotInitialized);
        }
        // Refresh readiness and media before constructing a new job.
        self.session.query_status()?;
        self.session
            .print_raster(lines, false, false, PrintQuality::Standard)
    }
    /// Close the RFCOMM channel and its exclusively owned baseband connection.
    ///
    /// Apple's baseband close API is synchronous. Drop also attempts cleanup;
    /// explicit close allows errors to be observed.
    pub fn close(self) -> Result<()> {
        self.session.close()
    }
}
