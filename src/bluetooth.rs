use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock, Weak};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Networking::WinSock::{
    closesocket, connect, recv, send, setsockopt, shutdown, socket, WSAGetLastError, WSAStartup,
    INVALID_SOCKET, SD_BOTH, SOCKET, SOCKET_ERROR, SOCK_STREAM, SOL_SOCKET, SO_RCVTIMEO,
    SO_SNDTIMEO, WSADATA,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExA, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use crate::protocol::{
    apply_packet, extract_packets, BudsState, PlacementState, SAMSUNG_SPP_GUID,
};

/// Lock a mutex, recovering the data if a worker thread panicked while holding it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const AF_BTH: i32 = 32;
const BTHPROTO_RFCOMM: i32 = 3;
const WSAETIMEDOUT: i32 = 10060;
const RECONNECT_DELAYS_S: [u64; 5] = [2, 4, 8, 15, 30];

#[repr(C, packed(1))]
struct SockAddrBth {
    address_family: u16,
    bt_addr: u64,
    service_class_id: [u8; 16],
    port: u32,
}

const _: () = assert!(size_of::<SockAddrBth>() == 30);

#[repr(C)]
struct SystemTime {
    _fields: [u16; 8],
}

#[repr(C)]
struct BluetoothDeviceInfo {
    dw_size: u32,
    address: u64,
    ul_class_of_device: u32,
    f_connected: i32,
    f_remembered: i32,
    f_authenticated: i32,
    st_last_seen: SystemTime,
    st_last_used: SystemTime,
    sz_name: [u16; 248],
}

#[repr(C)]
struct BluetoothDeviceSearchParams {
    dw_size: u32,
    f_return_authenticated: i32,
    f_return_remembered: i32,
    f_return_unknown: i32,
    f_return_connected: i32,
    f_issue_inquiry: i32,
    c_timeout_multiplier: u8,
    h_radio: *mut c_void,
}

type FnFindFirst = unsafe extern "system" fn(*const BluetoothDeviceSearchParams, *mut BluetoothDeviceInfo) -> *mut c_void;
type FnFindNext = unsafe extern "system" fn(*mut c_void, *mut BluetoothDeviceInfo) -> i32;
type FnFindClose = unsafe extern "system" fn(*mut c_void) -> i32;

#[derive(Debug, Clone)]
pub struct BtDeviceInfo {
    pub name: String,
    pub address: u64,
    pub is_connected: bool,
    pub class_of_device: u32,
}

impl BtDeviceInfo {
    fn is_buds(&self) -> bool {
        self.name.to_ascii_lowercase().contains("buds")
    }

    /// Major device class "Audio/Video" (headsets, earbuds, speakers).
    fn is_audio(&self) -> bool {
        (self.class_of_device >> 8) & 0x1F == 0x04
    }
}

/// Devices worth an RFCOMM attempt, best first: name matches, then connected audio devices
/// (covers renamed buds). Mice, keyboards and phones are never tried.
pub fn rank_candidates(devices: Vec<BtDeviceInfo>) -> Vec<BtDeviceInfo> {
    let mut candidates: Vec<BtDeviceInfo> =
        devices.into_iter().filter(|d| d.is_buds() || (d.is_connected && d.is_audio())).collect();
    candidates.sort_by_key(|d| (!d.is_buds(), !d.is_connected));
    candidates
}

fn ensure_wsa() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        let mut data: WSADATA = zeroed();
        let rc = WSAStartup(0x0202, &mut data);
        if rc != 0 {
            eprintln!("WSAStartup failed: {rc}");
        }
    });
}

#[derive(Clone, Copy)]
struct BthApi {
    find_first: FnFindFirst,
    find_next: FnFindNext,
    find_close: FnFindClose,
}

/// Resolve the bthprops entry points once (System32 only, never the app/CWD search path).
fn bth_api() -> Option<BthApi> {
    static API: OnceLock<Option<BthApi>> = OnceLock::new();
    *API.get_or_init(|| unsafe {
        // Load bthprops.cpl dynamically so both MSVC and GNU Rust toolchains link cleanly without libbthprops.a
        let module = LoadLibraryExA(
            c"bthprops.cpl".as_ptr().cast(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module.is_null() {
            return None;
        }
        let find_first: Option<FnFindFirst> =
            GetProcAddress(module, c"BluetoothFindFirstDevice".as_ptr().cast()).map(|f| std::mem::transmute(f));
        let find_next: Option<FnFindNext> =
            GetProcAddress(module, c"BluetoothFindNextDevice".as_ptr().cast()).map(|f| std::mem::transmute(f));
        let find_close: Option<FnFindClose> =
            GetProcAddress(module, c"BluetoothFindDeviceClose".as_ptr().cast()).map(|f| std::mem::transmute(f));
        Some(BthApi { find_first: find_first?, find_next: find_next?, find_close: find_close? })
    })
}

pub fn find_paired_devices() -> Vec<BtDeviceInfo> {
    let mut devices = Vec::new();
    let Some(BthApi { find_first, find_next, find_close }) = bth_api() else {
        return devices;
    };
    unsafe {

        let mut sp: BluetoothDeviceSearchParams = zeroed();
        sp.dw_size = size_of::<BluetoothDeviceSearchParams>() as u32;
        sp.f_return_authenticated = 1;
        sp.f_return_remembered = 1;
        sp.f_return_connected = 1;

        let mut info: BluetoothDeviceInfo = zeroed();
        info.dw_size = size_of::<BluetoothDeviceInfo>() as u32;

        let handle = find_first(&sp, &mut info);
        if handle.is_null() {
            return devices;
        }

        loop {
            let end = info.sz_name.iter().position(|&c| c == 0).unwrap_or(info.sz_name.len());
            let name = String::from_utf16_lossy(&info.sz_name[..end]);
            devices.push(BtDeviceInfo {
                name,
                address: info.address,
                is_connected: info.f_connected != 0,
                class_of_device: info.ul_class_of_device,
            });
            if find_next(handle, &mut info) == 0 {
                break;
            }
        }
        find_close(handle);
    }
    devices
}

#[derive(Clone)]
pub struct ConnectionSnapshot {
    pub connected: bool,
    pub connecting: bool,
    pub device_name: String,
    /// Bluetooth address of the last successfully connected device (kept after a drop).
    pub device_address: Option<u64>,
    pub status_text: String,
    pub state: BudsState,
}

struct Inner {
    socket: Mutex<SOCKET>,
    /// Socket currently inside a blocking `connect()`, so `disconnect()` can abort it.
    pending: Mutex<SOCKET>,
    stop_flag: Mutex<Option<Arc<AtomicBool>>>,
    snapshot: Mutex<ConnectionSnapshot>,
    on_update: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Feeds the single ordered writer thread.
    tx: Mutex<Sender<Vec<u8>>>,
    /// Set once the app is exiting so background reconnect attempts stop.
    shutdown: AtomicBool,
    /// True while an auto_reconnect loop is running, so drops can't stack up loops.
    reconnecting: AtomicBool,
}

#[derive(Clone)]
pub struct BudsClient {
    inner: Arc<Inner>,
}

/// Clears `connecting` on every exit path of the connect worker (including panics).
struct ConnectingGuard {
    inner: Arc<Inner>,
    armed: bool,
}

impl ConnectingGuard {
    /// Call once the worker has set `connecting` itself; a later drop must not touch it.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.inner.snapshot).connecting = false;
        }
    }
}

/// Reset everything that came from the device so a disconnected UI shows no stale values.
fn clear_device_state(state: &mut BudsState) {
    state.placement_l = PlacementState::Disconnected;
    state.placement_r = PlacementState::Disconnected;
    state.battery_l = 0;
    state.battery_r = 0;
    state.battery_case = 0;
    state.is_left_charging = false;
    state.is_right_charging = false;
    state.is_case_charging = false;
    // Settings must be re-read from the buds before the UI trusts them again.
    state.has_initial_sync = false;
}

/// Writer thread: sends queued packets in order. Exits when the client is dropped.
fn writer_loop(rx: Receiver<Vec<u8>>, inner: Weak<Inner>) {
    while let Ok(packet) = rx.recv() {
        let Some(inner) = inner.upgrade() else { return };
        // Hold the lock while sending so the socket can't be closed (or its handle reused) mid-send.
        let guard = lock(&inner.socket);
        if *guard == INVALID_SOCKET {
            continue;
        }
        let mut off = 0usize;
        while off < packet.len() {
            let n = unsafe { send(*guard, packet.as_ptr().add(off), (packet.len() - off) as i32, 0) };
            if n == SOCKET_ERROR || n <= 0 {
                break; // drop the rest of this packet; the reader notices a dead link
            }
            off += n as usize;
        }
    }
}

impl BudsClient {
    pub fn new() -> Self {
        ensure_wsa();
        let (tx, rx) = channel::<Vec<u8>>();
        let inner = Arc::new(Inner {
            socket: Mutex::new(INVALID_SOCKET),
            pending: Mutex::new(INVALID_SOCKET),
            stop_flag: Mutex::new(None),
            snapshot: Mutex::new(ConnectionSnapshot {
                connected: false,
                connecting: false,
                device_name: "Searching for Galaxy Buds...".into(),
                device_address: None,
                status_text: "Connecting over Bluetooth RFCOMM...".into(),
                state: BudsState::default(),
            }),
            on_update: Mutex::new(None),
            tx: Mutex::new(tx),
            shutdown: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&inner);
        thread::spawn(move || writer_loop(rx, weak));
        Self { inner }
    }

    pub fn set_on_update(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.inner.on_update) = Some(cb);
    }

    fn notify(&self) {
        // Clone the callback out so it never runs under the lock.
        let cb = lock(&self.inner.on_update).clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn snapshot(&self) -> ConnectionSnapshot {
        lock(&self.inner.snapshot).clone()
    }

    pub fn is_connected(&self) -> bool {
        *lock(&self.inner.socket) != INVALID_SOCKET
    }

    pub fn connect_auto_async(&self) {
        {
            let mut snap = lock(&self.inner.snapshot);
            if snap.connecting {
                return;
            }
            snap.connecting = true;
            snap.status_text = "Connecting over Bluetooth RFCOMM...".into();
        }
        self.notify();

        let client = self.clone();
        thread::spawn(move || {
            let mut guard = ConnectingGuard { inner: client.inner.clone(), armed: true };
            client.disconnect_internal(false);

            let candidates = rank_candidates(find_paired_devices());

            if candidates.is_empty() {
                let mut snap = lock(&client.inner.snapshot);
                snap.connecting = false;
                guard.disarm();
                snap.connected = false;
                snap.device_name = "Galaxy Buds".into();
                snap.status_text = "No connected Galaxy Buds found".into();
                drop(snap);
                client.notify();
                return;
            }

            for dev in &candidates {
                if client.inner.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(sock) = try_connect_device(&client.inner, dev) {
                    let stop = Arc::new(AtomicBool::new(false));
                    {
                        // Re-check shutdown under the socket lock: disconnect() sets it before locking.
                        let mut sg = lock(&client.inner.socket);
                        if client.inner.shutdown.load(Ordering::SeqCst) {
                            drop(sg);
                            unsafe {
                                closesocket(sock);
                            }
                            return;
                        }
                        *lock(&client.inner.stop_flag) = Some(stop.clone());
                        *sg = sock;
                    }
                    {
                        let mut snap = lock(&client.inner.snapshot);
                        snap.connecting = false;
                        guard.disarm();
                        snap.connected = true;
                        snap.device_name = dev.name.clone();
                        snap.device_address = Some(dev.address);
                        snap.status_text = format!("Connected to {}", dev.name);
                    }
                    client.notify();

                    let reader_client = client.clone();
                    thread::spawn(move || reader_client.read_loop(sock, stop));
                    return;
                }
            }

            if client.inner.shutdown.load(Ordering::SeqCst) {
                return; // guard clears `connecting`
            }
            let mut snap = lock(&client.inner.snapshot);
            snap.connecting = false;
            guard.disarm();
            snap.connected = false;
            snap.device_name = candidates[0].name.clone();
            snap.status_text = format!("Could not connect to {}", candidates[0].name);
            drop(snap);
            client.notify();
        });
    }

    /// Queue a packet for the single ordered writer thread.
    pub fn send_async(&self, packet: Vec<u8>) {
        let _ = lock(&self.inner.tx).send(packet);
    }

    /// Queue a packet and apply its expected effect to the local state right away, so the UI
    /// doesn't snap back to the old value while waiting for the buds' ACK (which overwrites it
    /// with the real value). Does nothing while disconnected, since the packet would be dropped.
    pub fn send_with(&self, packet: Vec<u8>, apply: impl FnOnce(&mut BudsState)) {
        {
            let mut snap = lock(&self.inner.snapshot);
            if !snap.connected {
                return;
            }
            apply(&mut snap.state);
        }
        self.send_async(packet);
        self.notify();
    }

    pub fn disconnect(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        // Abort a connect() that is blocking on another thread.
        let pending = {
            let mut p = lock(&self.inner.pending);
            std::mem::replace(&mut *p, INVALID_SOCKET)
        };
        if pending != INVALID_SOCKET {
            unsafe {
                closesocket(pending);
            }
        }
        self.disconnect_internal(true);
    }

    fn disconnect_internal(&self, update_status: bool) {
        if let Some(flag) = lock(&self.inner.stop_flag).take() {
            flag.store(true, Ordering::SeqCst);
        }
        let sock = {
            let mut guard = lock(&self.inner.socket);
            std::mem::replace(&mut *guard, INVALID_SOCKET)
        };
        if sock != INVALID_SOCKET {
            unsafe {
                shutdown(sock, SD_BOTH);
                closesocket(sock);
            }
        }
        {
            let mut snap = lock(&self.inner.snapshot);
            snap.connected = false;
            clear_device_state(&mut snap.state);
            if update_status {
                snap.status_text = "Disconnected — click Reconnect".into();
            }
        }
        if update_status {
            self.notify();
        }
    }

    fn read_loop(&self, sock: SOCKET, stop: Arc<AtomicBool>) {
        let mut rx_buf = [0u8; 4096];
        let mut stream_buf: Vec<u8> = Vec::with_capacity(4096);

        while !stop.load(Ordering::SeqCst) {
            let n = unsafe { recv(sock, rx_buf.as_mut_ptr(), rx_buf.len() as i32, 0) };
            if n > 0 {
                stream_buf.extend_from_slice(&rx_buf[..n as usize]);
                let packets = extract_packets(&mut stream_buf);
                let mut changed = false;
                {
                    let mut snap = lock(&self.inner.snapshot);
                    for pkt in &packets {
                        if apply_packet(&mut snap.state, pkt) {
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.notify();
                }
            } else if n == 0 {
                break;
            } else {
                let err = unsafe { WSAGetLastError() };
                if err == WSAETIMEDOUT {
                    continue;
                }
                break;
            }
        }

        if stop.load(Ordering::SeqCst) {
            return;
        }
        // Unexpected drop. Only tear down if the socket is still *ours*: a manual reconnect may
        // already have replaced it, and we must not close the new connection or clear its stop flag.
        let owned = {
            let mut guard = lock(&self.inner.socket);
            if *guard == sock && !stop.load(Ordering::SeqCst) {
                *guard = INVALID_SOCKET;
                true
            } else {
                false
            }
        };
        if !owned {
            return;
        }
        unsafe {
            shutdown(sock, SD_BOTH);
            closesocket(sock);
        }
        {
            let mut sf = lock(&self.inner.stop_flag);
            if sf.as_ref().is_some_and(|f| Arc::ptr_eq(f, &stop)) {
                *sf = None;
            }
        }
        {
            let mut snap = lock(&self.inner.snapshot);
            snap.connected = false;
            clear_device_state(&mut snap.state);
        }
        self.notify();
        if !self.inner.shutdown.load(Ordering::SeqCst) && !self.inner.reconnecting.swap(true, Ordering::SeqCst) {
            let client = self.clone();
            thread::spawn(move || {
                client.auto_reconnect();
                client.inner.reconnecting.store(false, Ordering::SeqCst);
            });
        }
    }

    /// Retries with increasing delays after an unexpected drop (buds put in case, out of range, etc.).
    fn auto_reconnect(&self) {
        for delay_s in RECONNECT_DELAYS_S {
            if self.inner.shutdown.load(Ordering::SeqCst) || self.is_connected() {
                return;
            }
            self.set_status(format!("Connection lost — retrying in {delay_s}s..."));
            // Sleep in short slices so shutdown or a manual reconnect is noticed promptly.
            for _ in 0..delay_s * 5 {
                thread::sleep(Duration::from_millis(200));
                if self.inner.shutdown.load(Ordering::SeqCst) || self.is_connected() {
                    return;
                }
            }
            self.connect_auto_async();
            // Wait for the attempt (ours or one the user started) to finish.
            while self.snapshot().connecting {
                if self.inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if self.is_connected() {
                return;
            }
        }
        if !self.inner.shutdown.load(Ordering::SeqCst) && !self.is_connected() {
            self.set_status("Disconnected — click Reconnect".into());
        }
    }

    fn set_status(&self, text: String) {
        lock(&self.inner.snapshot).status_text = text;
        self.notify();
    }
}

fn set_timeout(sock: SOCKET, opt: i32, ms: i32) -> bool {
    unsafe { setsockopt(sock, SOL_SOCKET, opt, (&ms as *const i32).cast(), size_of::<i32>() as i32) == 0 }
}

fn try_connect_device(inner: &Inner, dev: &BtDeviceInfo) -> Option<SOCKET> {
    // Retry once after a short delay in case earbuds are still releasing a just-closed RFCOMM channel
    for attempt in 0..2 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(450));
        }
        if inner.shutdown.load(Ordering::SeqCst) {
            return None;
        }
        unsafe {
            let sock = socket(AF_BTH, SOCK_STREAM, BTHPROTO_RFCOMM);
            if sock == INVALID_SOCKET {
                return None;
            }
            // Receive timeout lets the reader poll its stop flag; the send timeout bounds blocking
            // sends so a stalled link can't hold the socket lock (and block disconnect) forever.
            if !set_timeout(sock, SO_RCVTIMEO, 1500) || !set_timeout(sock, SO_SNDTIMEO, 1500) {
                closesocket(sock);
                return None;
            }

            // Publish the in-flight socket so disconnect() can close it and abort connect().
            {
                let mut p = lock(&inner.pending);
                if inner.shutdown.load(Ordering::SeqCst) {
                    drop(p);
                    closesocket(sock);
                    return None;
                }
                *p = sock;
            }

            let addr = SockAddrBth {
                address_family: AF_BTH as u16,
                bt_addr: dev.address,
                service_class_id: SAMSUNG_SPP_GUID,
                port: 0,
            };

            let rc = connect(sock, (&addr as *const SockAddrBth).cast(), size_of::<SockAddrBth>() as i32);

            // Reclaim ownership; if disconnect() already took (and closed) it, the attempt is dead.
            let still_ours = {
                let mut p = lock(&inner.pending);
                if *p == sock {
                    *p = INVALID_SOCKET;
                    true
                } else {
                    false
                }
            };
            if !still_ours {
                return None;
            }
            if rc == 0 {
                return Some(sock);
            }
            closesocket(sock);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const COD_HEADSET: u32 = 0x24_0404; // Audio/Video major class, wearable headset
    const COD_MOUSE: u32 = 0x00_2580; // Peripheral major class, pointing device
    const COD_PHONE: u32 = 0x5A_020C; // Phone major class

    fn dev(name: &str, connected: bool, cod: u32) -> BtDeviceInfo {
        BtDeviceInfo { name: name.into(), address: 0, is_connected: connected, class_of_device: cod }
    }

    fn names(v: &[BtDeviceInfo]) -> Vec<&str> {
        v.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn ranks_buds_first_and_skips_non_audio_devices() {
        let ranked = rank_candidates(vec![
            dev("MX Master 3", true, COD_MOUSE),
            dev("JBL Charge 3", true, COD_HEADSET),
            dev("Pixel 9", true, COD_PHONE),
            dev("Old Galaxy Buds", false, COD_HEADSET),
            dev("Galaxy Buds4 Pro", true, COD_HEADSET),
        ]);
        assert_eq!(names(&ranked), ["Galaxy Buds4 Pro", "Old Galaxy Buds", "JBL Charge 3"]);
    }

    #[test]
    fn keeps_renamed_buds_when_connected_audio() {
        let ranked = rank_candidates(vec![
            dev("My earphones", true, COD_HEADSET),
            dev("Speaker in kitchen", false, COD_HEADSET),
        ]);
        assert_eq!(names(&ranked), ["My earphones"]);
    }

    #[test]
    fn clearing_device_state_forces_resync() {
        let mut st = BudsState { battery_l: 80, has_initial_sync: true, ..BudsState::default() };
        st.placement_l = PlacementState::Wearing;
        clear_device_state(&mut st);
        assert_eq!(st.battery_l, 0);
        assert_eq!(st.placement_l, PlacementState::Disconnected);
        assert!(!st.has_initial_sync);
    }
}
