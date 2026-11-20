use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Networking::WinSock::{
    closesocket, connect, recv, send, setsockopt, shutdown, socket, WSAGetLastError, WSAStartup,
    INVALID_SOCKET, SD_BOTH, SOCKET, SOCK_STREAM, SOL_SOCKET, SO_RCVTIMEO, WSADATA,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

use crate::protocol::{apply_packet, extract_packets, BudsState, SAMSUNG_SPP_GUID};

const AF_BTH: i32 = 32;
const BTHPROTO_RFCOMM: i32 = 3;
const WSAETIMEDOUT: i32 = 10060;

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
}

fn ensure_wsa() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        let mut data: WSADATA = zeroed();
        WSAStartup(0x0202, &mut data);
    });
}

pub fn find_paired_devices() -> Vec<BtDeviceInfo> {
    let mut devices = Vec::new();
    unsafe {
        // Load bthprops.cpl dynamically so both MSVC and GNU Rust toolchains link cleanly without libbthprops.a
        let module = LoadLibraryA(b"bthprops.cpl\0".as_ptr());
        if module.is_null() {
            return devices;
        }
        let find_first: Option<FnFindFirst> =
            GetProcAddress(module, b"BluetoothFindFirstDevice\0".as_ptr()).map(|f| std::mem::transmute(f));
        let find_next: Option<FnFindNext> =
            GetProcAddress(module, b"BluetoothFindNextDevice\0".as_ptr()).map(|f| std::mem::transmute(f));
        let find_close: Option<FnFindClose> =
            GetProcAddress(module, b"BluetoothFindDeviceClose\0".as_ptr()).map(|f| std::mem::transmute(f));

        let (Some(find_first), Some(find_next), Some(find_close)) = (find_first, find_next, find_close) else {
            return devices;
        };

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
    pub status_text: String,
    pub state: BudsState,
}

struct Inner {
    socket: Mutex<SOCKET>,
    stop_flag: Mutex<Option<Arc<AtomicBool>>>,
    snapshot: Mutex<ConnectionSnapshot>,
    on_update: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone)]
pub struct BudsClient {
    inner: Arc<Inner>,
}

impl BudsClient {
    pub fn new() -> Self {
        ensure_wsa();
        Self {
            inner: Arc::new(Inner {
                socket: Mutex::new(INVALID_SOCKET),
                stop_flag: Mutex::new(None),
                snapshot: Mutex::new(ConnectionSnapshot {
                    connected: false,
                    connecting: false,
                    device_name: "Searching for Galaxy Buds...".into(),
                    status_text: "Connecting over Bluetooth RFCOMM...".into(),
                    state: BudsState::default(),
                }),
                on_update: Mutex::new(None),
            }),
        }
    }

    pub fn set_on_update(&self, cb: Arc<dyn Fn() + Send + Sync>) {
        *self.inner.on_update.lock().unwrap() = Some(cb);
    }

    fn notify(&self) {
        if let Some(cb) = self.inner.on_update.lock().unwrap().as_ref() {
            cb();
        }
    }

    pub fn snapshot(&self) -> ConnectionSnapshot {
        self.inner.snapshot.lock().unwrap().clone()
    }

    pub fn is_connected(&self) -> bool {
        *self.inner.socket.lock().unwrap() != INVALID_SOCKET
    }

    pub fn connect_auto_async(&self) {
        {
            let mut snap = self.inner.snapshot.lock().unwrap();
            if snap.connecting {
                return;
            }
            snap.connecting = true;
            snap.status_text = "Connecting over Bluetooth RFCOMM...".into();
        }
        self.notify();

        let client = self.clone();
        thread::spawn(move || {
            client.disconnect_internal(false);

            let mut candidates: Vec<BtDeviceInfo> = find_paired_devices()
                .into_iter()
                .filter(|d| d.is_connected || d.name.to_ascii_lowercase().contains("buds"))
                .collect();

            candidates.sort_by_key(|d| (!d.is_connected, !d.name.to_ascii_lowercase().contains("buds")));

            if candidates.is_empty() {
                let mut snap = client.inner.snapshot.lock().unwrap();
                snap.connecting = false;
                snap.connected = false;
                snap.device_name = "Galaxy Buds".into();
                snap.status_text = "No connected Galaxy Buds found".into();
                drop(snap);
                client.notify();
                return;
            }

            for dev in &candidates {
                if let Some(sock) = try_connect_device(dev) {
                    let stop = Arc::new(AtomicBool::new(false));
                    *client.inner.socket.lock().unwrap() = sock;
                    *client.inner.stop_flag.lock().unwrap() = Some(stop.clone());
                    {
                        let mut snap = client.inner.snapshot.lock().unwrap();
                        snap.connecting = false;
                        snap.connected = true;
                        snap.device_name = dev.name.clone();
                        snap.status_text = format!("Connected to {}", dev.name);
                    }
                    client.notify();

                    let reader_client = client.clone();
                    thread::spawn(move || reader_client.read_loop(sock, stop));
                    return;
                }
            }

            let mut snap = client.inner.snapshot.lock().unwrap();
            snap.connecting = false;
            snap.connected = false;
            snap.device_name = candidates[0].name.clone();
            snap.status_text = format!("Could not connect to {}", candidates[0].name);
            drop(snap);
            client.notify();
        });
    }

    pub fn send_async(&self, packet: Vec<u8>) {
        let client = self.clone();
        thread::spawn(move || {
            let sock = *client.inner.socket.lock().unwrap();
            if sock == INVALID_SOCKET {
                return;
            }
            unsafe {
                send(sock, packet.as_ptr(), packet.len() as i32, 0);
            }
        });
    }

    pub fn disconnect(&self) {
        self.disconnect_internal(true);
    }

    fn disconnect_internal(&self, update_status: bool) {
        if let Some(flag) = self.inner.stop_flag.lock().unwrap().take() {
            flag.store(true, Ordering::SeqCst);
        }
        let sock = {
            let mut guard = self.inner.socket.lock().unwrap();
            let s = *guard;
            *guard = INVALID_SOCKET;
            s
        };
        if sock != INVALID_SOCKET {
            unsafe {
                shutdown(sock, SD_BOTH);
                closesocket(sock);
            }
        }
        if update_status {
            let mut snap = self.inner.snapshot.lock().unwrap();
            snap.connected = false;
            snap.status_text = "Disconnected — click Reconnect".into();
            drop(snap);
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
                    let mut snap = self.inner.snapshot.lock().unwrap();
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

        if !stop.load(Ordering::SeqCst) {
            self.disconnect_internal(true);
        }
    }
}

fn try_connect_device(dev: &BtDeviceInfo) -> Option<SOCKET> {
    // Retry once after a short delay in case earbuds are still releasing a just-closed RFCOMM channel
    for attempt in 0..2 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(450));
        }
        unsafe {
            let sock = socket(AF_BTH, SOCK_STREAM, BTHPROTO_RFCOMM);
            if sock == INVALID_SOCKET {
                return None;
            }
            let timeout_ms: i32 = 1500;
            setsockopt(
                sock,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&timeout_ms as *const i32).cast(),
                size_of::<i32>() as i32,
            );

            let addr = SockAddrBth {
                address_family: AF_BTH as u16,
                bt_addr: dev.address,
                service_class_id: SAMSUNG_SPP_GUID,
                port: 0,
            };

            if connect(sock, (&addr as *const SockAddrBth).cast(), size_of::<SockAddrBth>() as i32) == 0 {
                return Some(sock);
            }
            closesocket(sock);
        }
    }
    None
}
