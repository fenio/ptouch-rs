// SPDX-License-Identifier: GPL-3.0-or-later
//! Native macOS RFCOMM transport. All objects stay on the main thread.
use crate::{
    error::{PtouchError, Result},
    session::Transport,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
fn bt(message: impl Into<String>) -> PtouchError {
    PtouchError::Bluetooth(message.into())
}

use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, rc::Retained};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSRunLoop, NSString,
};
use objc2_io_bluetooth::{IOBluetoothDevice, IOBluetoothRFCOMMChannel, IOBluetoothSDPUUID};
use std::{
    cell::{Cell, RefCell},
    ffi::{c_int, c_void},
    marker::PhantomData,
    rc::Rc,
};
#[derive(Default)]
struct State {
    overflow: Cell<bool>,
    opened: Cell<Option<c_int>>,
    closed: Cell<bool>,
    rx: RefCell<VecDeque<u8>>,
}
struct Write {
    bytes: Vec<u8>,
    result: Cell<Option<c_int>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PtouchNativeBluetoothDelegate"]
    #[ivars = State]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    impl Delegate {
        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        fn opened(&self, _channel:Option<&IOBluetoothRFCOMMChannel>, status:c_int) { self.ivars().opened.set(Some(status)); }
        #[unsafe(method(rfcommChannelClosed:))]
        fn closed(&self, _channel:Option<&IOBluetoothRFCOMMChannel>) { self.ivars().closed.set(true); }
        #[unsafe(method(rfcommChannelData:data:length:))]
        fn data(&self, _channel:Option<&IOBluetoothRFCOMMChannel>, data:*mut c_void, length:usize) {
            if length>0 && !data.is_null() {
                // Native callback data is valid only for the duration of this call.
                let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(),length) };
                let mut rx = self.ivars().rx.borrow_mut();
                if bytes.len() > 65536usize.saturating_sub(rx.len()) {
                    self.ivars().overflow.set(true);
                } else { rx.extend(bytes); }
            }
        }
        #[unsafe(method(rfcommChannelWriteComplete:refcon:status:))]
        fn written(&self, _channel:Option<&IOBluetoothRFCOMMChannel>, refcon:*mut c_void, status:c_int) {
            if !refcon.is_null() { unsafe { &*refcon.cast::<Write>() }.result.set(Some(status)); }
        }
    }
);
impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(State::default());
        unsafe { msg_send![super(this), init] }
    }
}
fn pump() {
    let until = NSDate::dateWithTimeIntervalSinceNow(0.01);
    NSRunLoop::currentRunLoop().runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
}
fn wait(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + timeout;
    while !ready() {
        if Instant::now() >= until {
            return false;
        }
        pump();
    }
    true
}
pub(crate) struct NativeTransport {
    device: Retained<IOBluetoothDevice>,
    channel: Retained<IOBluetoothRFCOMMChannel>,
    delegate: Retained<Delegate>,
    // Keep native objects and all callbacks confined to the creating thread.
    _owner: PhantomData<Rc<()>>,
    closed: Cell<bool>,
}
impl NativeTransport {
    pub(crate) fn paired_devices() -> Result<Vec<(String, String)>> {
        let _main = MainThreadMarker::new()
            .ok_or_else(|| bt("Native Bluetooth must be queried on the main thread"))?;
        let Some(devices) = (unsafe { IOBluetoothDevice::pairedDevices() }) else {
            return Ok(Vec::new());
        };
        let mut paired = Vec::new();
        for index in 0..devices.len() {
            let object = devices.objectAtIndex(index);
            let Some(device) = object.downcast_ref::<IOBluetoothDevice>() else {
                continue;
            };
            let Some(address) = (unsafe { device.addressString() }) else {
                continue;
            };
            let address = normalize_address(&address.to_string());
            let name = unsafe { device.nameOrAddress() }
                .map(|name| name.to_string())
                .unwrap_or_else(|| address.clone());
            paired.push((name, address));
        }
        paired.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        Ok(paired)
    }

    pub(crate) fn open(address: &str) -> Result<Self> {
        let _main = MainThreadMarker::new()
            .ok_or_else(|| bt("Native Bluetooth must be opened and used on the main thread"))?;
        let parts: Vec<_> = address.split(':').collect();
        if parts.len() != 6
            || parts
                .iter()
                .any(|p| p.len() != 2 || !p.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(bt("Expected a Bluetooth address such as AA:BB:CC:DD:EE:FF"));
        }
        let address = NSString::from_str(address);
        let device = unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&address)) }
            .ok_or_else(|| bt("Unknown Bluetooth address"))?;
        if !unsafe { device.isPaired() } {
            return Err(bt("Pair the printer in macOS Bluetooth settings first"));
        }
        let uuid = unsafe { IOBluetoothSDPUUID::uuid16(0x1101) }
            .ok_or_else(|| bt("Cannot create SPP UUID"))?;
        if unsafe { device.getServiceRecordForUUID(Some(&uuid)) }.is_none() {
            let code = unsafe { device.performSDPQuery(None) };
            if code != 0 {
                return Err(bt(format!("SDP query failed: {code:#x}")));
            }
            if !wait(Duration::from_secs(8), || {
                unsafe { device.getServiceRecordForUUID(Some(&uuid)) }.is_some()
            }) {
                return Err(bt("SPP service discovery timed out"));
            }
        }
        let service = unsafe { device.getServiceRecordForUUID(Some(&uuid)) }
            .ok_or_else(|| bt("No SPP service"))?;
        let mut id = 0;
        let code = unsafe { service.getRFCOMMChannelID(&mut id) };
        if code != 0 || id == 0 {
            return Err(bt(format!("Cannot resolve RFCOMM channel: {code:#x}")));
        }
        let delegate = Delegate::new();
        let mut channel = None;
        let code = unsafe {
            device.openRFCOMMChannelAsync_withChannelID_delegate(
                Some(&mut channel),
                id,
                Some(&delegate),
            )
        };
        let channel = channel.ok_or_else(|| bt(format!("No RFCOMM channel: {code:#x}")))?;
        let conn = Self {
            device,
            channel,
            delegate,
            _owner: PhantomData,
            closed: Cell::new(false),
        };
        if code != 0 {
            return Err(bt(format!("RFCOMM open rejected: {code:#x}")));
        }
        if !wait(Duration::from_secs(15), || {
            conn.delegate.ivars().opened.get().is_some()
        }) {
            conn.shutdown()?;
            return Err(bt("RFCOMM connection timed out"));
        }
        let code = conn.delegate.ivars().opened.get().unwrap();
        if code != 0 {
            return Err(bt(format!("RFCOMM connection failed: {code:#x}")));
        }
        log::debug!("Connected: RFCOMM channel {id}, MTU {}", unsafe {
            conn.channel.getMTU()
        });
        Ok(conn)
    }
    fn write(&self, bytes: &[u8], timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        if self.closed.get() || self.delegate.ivars().closed.get() {
            return Err(bt("RFCOMM channel is closed"));
        }
        let mtu = usize::from(unsafe { self.channel.getMTU() }).clamp(1, 1024);
        for chunk in bytes.chunks(mtu) {
            if Instant::now() >= deadline {
                return Err(PtouchError::Timeout);
            }
            // A stable allocation stays alive until the asynchronous completion callback.
            let mut request = Box::new(Write {
                bytes: chunk.to_vec(),
                result: Cell::new(None),
            });
            let refcon = (&mut *request as *mut Write).cast();
            let code = unsafe {
                self.channel.writeAsync_length_refcon(
                    request.bytes.as_mut_ptr().cast(),
                    request.bytes.len() as u16,
                    refcon,
                )
            };
            if code != 0 {
                return Err(bt(format!("RFCOMM write rejected: {code:#x}")));
            }
            if !wait(deadline.saturating_duration_since(Instant::now()), || {
                request.result.get().is_some() || self.delegate.ivars().closed.get()
            }) || request.result.get().is_none()
            {
                // Cancellation does not document buffer lifetime. Preserve this allocation
                // if no callback arrived, avoiding use-after-free; never retry the print job.
                Box::leak(request);
                let _ = self.shutdown();
                return Err(bt(
                    "RFCOMM write timed out or disconnected (buffer retained)",
                ));
            }
            let code = request.result.get().unwrap();
            if code != 0 {
                return Err(bt(format!("RFCOMM write failed: {code:#x}")));
            }
        }
        Ok(())
    }
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.closed.get() || self.delegate.ivars().closed.get() {
            return Err(bt("RFCOMM disconnected while reading"));
        }
        if !wait(timeout, || {
            !self.delegate.ivars().rx.borrow().is_empty()
                || self.delegate.ivars().closed.get()
                || self.delegate.ivars().overflow.get()
        }) {
            return Err(PtouchError::Timeout);
        }
        if self.delegate.ivars().overflow.get() {
            let _ = self.shutdown();
            return Err(bt("Bluetooth receive buffer exceeded its limit"));
        }
        let mut rx = self.delegate.ivars().rx.borrow_mut();
        if rx.is_empty() && self.delegate.ivars().closed.get() {
            return Err(bt("RFCOMM disconnected while reading"));
        }
        let count = buf.len().min(rx.len());
        for byte in &mut buf[..count] {
            *byte = rx.pop_front().unwrap();
        }
        Ok(count)
    }
    fn shutdown(&self) -> Result<()> {
        if self.closed.get() {
            return Ok(());
        }
        self.closed.set(true);
        let code = unsafe { self.channel.closeChannel() };
        // Let native close notifications drain before detaching and reopening.
        wait(Duration::from_secs(1), || {
            self.delegate.ivars().closed.get()
        });
        unsafe { self.channel.setDelegate(None) };
        // This session exclusively owns the printer connection.
        // Closing RFCOMM alone left the next connection without an open callback.
        let baseband = unsafe { self.device.closeConnection() };
        if baseband != 0 {
            return Err(bt(format!("Baseband close failed: {baseband:#x}")));
        }
        let settle = Instant::now() + Duration::from_millis(250);
        while Instant::now() < settle {
            pump();
        }
        if code != 0 {
            return Err(bt(format!("RFCOMM close failed: {code:#x}")));
        }
        Ok(())
    }
}

fn normalize_address(address: &str) -> String {
    address.replace('-', ":").to_ascii_uppercase()
}
impl Drop for NativeTransport {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl Transport for NativeTransport {
    fn send(&self, bytes: &[u8], timeout: Duration) -> Result<()> {
        let result = self.write(bytes, timeout);
        if result.is_err() {
            let _ = self.shutdown();
        }
        result
    }
    fn receive(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        self.read(buf, timeout)
    }
    fn close(self) -> Result<()> {
        self.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_macos_addresses_for_cli_input() {
        assert_eq!(normalize_address("ec-79-49-61-cc-f8"), "EC:79:49:61:CC:F8");
    }

    #[test]
    fn rejects_worker_thread_before_accessing_bluetooth() {
        let rejected = std::thread::spawn(|| match NativeTransport::open("AA:BB:CC:DD:EE:FF") {
            Err(PtouchError::Bluetooth(message)) => message.contains("main thread"),
            _ => false,
        })
        .join()
        .unwrap();
        assert!(rejected);
    }
}
