using System.Runtime.InteropServices;

namespace Pods4;

public sealed record BtDeviceInfo(string Name, ulong Address, bool IsConnected);

public sealed class BudsConnection : IDisposable
{
    private const int AfBth = 32;
    private const int SockStream = 1;
    private const int BthProtoRfcomm = 3;
    private const int SolSocket = 0xFFFF;
    private const int SoRcvTimeo = 0x1006;
    private static readonly IntPtr InvalidSocket = new(-1);

    private readonly object _sendLock = new();
    private readonly BudsState _state = new();
    private IntPtr _socket = InvalidSocket;
    private CancellationTokenSource? _cts;
    private Task? _readTask;
    private bool _disposed;

    public bool IsConnected => _socket != InvalidSocket;
    public BtDeviceInfo? CurrentDevice { get; private set; }
    public BudsState CurrentState { get { lock (_state) return _state.Clone(); } }

    public event Action<BudsState>? StateChanged;
    public event Action<bool, string>? ConnectionChanged;

    static BudsConnection()
    {
        WSAStartup(0x0202, new byte[512]);
    }

    public static List<BtDeviceInfo> FindPairedDevices()
    {
        var list = new List<BtDeviceInfo>();
        var sp = new BLUETOOTH_DEVICE_SEARCH_PARAMS
        {
            dwSize = (uint)Marshal.SizeOf<BLUETOOTH_DEVICE_SEARCH_PARAMS>(),
            fReturnAuthenticated = true,
            fReturnRemembered = true,
            fReturnConnected = true
        };
        var info = new BLUETOOTH_DEVICE_INFO { dwSize = (uint)Marshal.SizeOf<BLUETOOTH_DEVICE_INFO>() };
        IntPtr h = BluetoothFindFirstDevice(ref sp, ref info);
        if (h == IntPtr.Zero) return list;
        try
        {
            do
            {
                list.Add(new BtDeviceInfo(info.szName, info.Address, info.fConnected));
            } while (BluetoothFindNextDevice(h, ref info));
        }
        finally
        {
            BluetoothFindDeviceClose(h);
        }
        return list;
    }

    public static BtDeviceInfo? PickBestBudsDevice()
    {
        var devices = FindPairedDevices();
        // Prefer connected device with "Buds" in name, then any connected device, then paired "Buds"
        return devices.FirstOrDefault(d => d.IsConnected && d.Name.Contains("Buds", StringComparison.OrdinalIgnoreCase))
            ?? devices.FirstOrDefault(d => d.IsConnected)
            ?? devices.FirstOrDefault(d => d.Name.Contains("Buds", StringComparison.OrdinalIgnoreCase));
    }

    public Task<bool> ConnectAutoAsync()
    {
        return Task.Run(() =>
        {
            Disconnect();
            var devices = FindPairedDevices();
            // Order candidates: connected Buds -> connected others -> paired Buds
            var candidates = devices
                .OrderByDescending(d => d.IsConnected)
                .ThenByDescending(d => d.Name.Contains("Buds", StringComparison.OrdinalIgnoreCase))
                .Where(d => d.IsConnected || d.Name.Contains("Buds", StringComparison.OrdinalIgnoreCase))
                .ToList();

            if (candidates.Count == 0)
            {
                ConnectionChanged?.Invoke(false, "No connected Galaxy Buds found");
                return false;
            }

            foreach (var dev in candidates)
            {
                if (TryConnectDevice(dev))
                    return true;
            }

            ConnectionChanged?.Invoke(false, $"Could not connect to {candidates[0].Name}");
            return false;
        });
    }

    private bool TryConnectDevice(BtDeviceInfo dev)
    {
        // Retry once after a short delay in case the earbuds are still tearing down a just-closed RFCOMM link
        for (int attempt = 0; attempt < 2; attempt++)
        {
            if (attempt > 0) Thread.Sleep(450);

            IntPtr sock = socket(AfBth, SockStream, BthProtoRfcomm);
            if (sock == InvalidSocket) return false;

            int timeoutMs = 1500;
            setsockopt(sock, SolSocket, SoRcvTimeo, ref timeoutMs, sizeof(int));

            var addr = new SOCKADDR_BTH
            {
                addressFamily = AfBth,
                btAddr = dev.Address,
                serviceClassId = BudsProtocol.SamsungSppUuid,
                port = 0
            };

            if (connect(sock, ref addr, Marshal.SizeOf<SOCKADDR_BTH>()) != 0)
            {
                closesocket(sock);
                continue;
            }

            _socket = sock;
            CurrentDevice = dev;
            _cts = new CancellationTokenSource();
            ConnectionChanged?.Invoke(true, $"Connected to {dev.Name}");
            _readTask = Task.Run(() => ReadLoop(sock, _cts.Token));
            return true;
        }
        return false;
    }

    public Task<bool> SendAsync(byte[] packet)
    {
        return Task.Run(() =>
        {
            lock (_sendLock)
            {
                IntPtr sock = _socket;
                if (sock == InvalidSocket) return false;
                int sent = send(sock, packet, packet.Length, 0);
                return sent == packet.Length;
            }
        });
    }

    private void ReadLoop(IntPtr sock, CancellationToken ct)
    {
        var rxBuf = new byte[4096];
        var streamBuf = new List<byte>(4096);

        while (!ct.IsCancellationRequested)
        {
            int n = recv(sock, rxBuf, rxBuf.Length, 0);
            if (n > 0)
            {
                for (int i = 0; i < n; i++)
                    streamBuf.Add(rxBuf[i]);

                var packets = BudsProtocol.ExtractPackets(streamBuf);
                bool changed = false;
                BudsState? snapshot = null;
                lock (_state)
                {
                    foreach (var pkt in packets)
                    {
                        if (BudsProtocol.ApplyPacket(_state, pkt))
                            changed = true;
                    }
                    if (changed)
                        snapshot = _state.Clone();
                }

                if (snapshot != null)
                    StateChanged?.Invoke(snapshot);
            }
            else if (n == 0)
            {
                break; // Remote closed cleanly
            }
            else
            {
                int err = WSAGetLastError();
                if (err == 10060) // WSAETIMEDOUT: normal polling timeout so ct can be checked
                    continue;
                break;
            }
        }

        if (!ct.IsCancellationRequested)
        {
            Disconnect();
            ConnectionChanged?.Invoke(false, "Disconnected — click Reconnect");
        }
    }

    public void Disconnect()
    {
        var cts = Interlocked.Exchange(ref _cts, null);
        cts?.Cancel();

        IntPtr sock;
        lock (_sendLock)
        {
            sock = _socket;
            _socket = InvalidSocket;
        }
        if (sock != InvalidSocket)
        {
            shutdown(sock, 2); // SD_BOTH: cleanly tear down RFCOMM channel immediately
            closesocket(sock);
        }

        cts?.Dispose();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Disconnect();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 1)]
    private struct SOCKADDR_BTH
    {
        public ushort addressFamily;
        public ulong btAddr;
        public Guid serviceClassId;
        public uint port;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct SYSTEMTIME { public ushort y, m, dow, d, h, min, s, ms; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct BLUETOOTH_DEVICE_INFO
    {
        public uint dwSize;
        public ulong Address;
        public uint ulClassofDevice;
        [MarshalAs(UnmanagedType.Bool)] public bool fConnected;
        [MarshalAs(UnmanagedType.Bool)] public bool fRemembered;
        [MarshalAs(UnmanagedType.Bool)] public bool fAuthenticated;
        public SYSTEMTIME stLastSeen;
        public SYSTEMTIME stLastUsed;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 248)] public string szName;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BLUETOOTH_DEVICE_SEARCH_PARAMS
    {
        public uint dwSize;
        [MarshalAs(UnmanagedType.Bool)] public bool fReturnAuthenticated;
        [MarshalAs(UnmanagedType.Bool)] public bool fReturnRemembered;
        [MarshalAs(UnmanagedType.Bool)] public bool fReturnUnknown;
        [MarshalAs(UnmanagedType.Bool)] public bool fReturnConnected;
        [MarshalAs(UnmanagedType.Bool)] public bool fIssueInquiry;
        public byte cTimeoutMultiplier;
        public IntPtr hRadio;
    }

    [DllImport("ws2_32.dll")] private static extern int WSAStartup(ushort ver, byte[] data);
    [DllImport("ws2_32.dll")] private static extern IntPtr socket(int af, int type, int proto);
    [DllImport("ws2_32.dll")] private static extern int connect(IntPtr s, ref SOCKADDR_BTH addr, int namelen);
    [DllImport("ws2_32.dll")] private static extern int send(IntPtr s, byte[] buf, int len, int flags);
    [DllImport("ws2_32.dll")] private static extern int recv(IntPtr s, byte[] buf, int len, int flags);
    [DllImport("ws2_32.dll")] private static extern int setsockopt(IntPtr s, int level, int optname, ref int optval, int optlen);
    [DllImport("ws2_32.dll")] private static extern int shutdown(IntPtr s, int how);
    [DllImport("ws2_32.dll")] private static extern int closesocket(IntPtr s);
    [DllImport("ws2_32.dll")] private static extern int WSAGetLastError();

    [DllImport("bthprops.cpl")] private static extern IntPtr BluetoothFindFirstDevice(ref BLUETOOTH_DEVICE_SEARCH_PARAMS p, ref BLUETOOTH_DEVICE_INFO info);
    [DllImport("bthprops.cpl")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool BluetoothFindNextDevice(IntPtr h, ref BLUETOOTH_DEVICE_INFO info);
    [DllImport("bthprops.cpl")][return: MarshalAs(UnmanagedType.Bool)] private static extern bool BluetoothFindDeviceClose(IntPtr h);
}
