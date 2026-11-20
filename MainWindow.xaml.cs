using System.ComponentModel;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using Forms = System.Windows.Forms;

namespace Pods4;

public partial class MainWindow : Window
{
    private readonly BudsConnection _conn = new();
    private readonly Forms.NotifyIcon _tray;
    private bool _syncing;
    private bool _exiting;
    private int _lastAmbientVol = -1;
    private int _lastAncLevel = -1;

    private static readonly string[] BoostLabels = ["-1", "Normal", "+1"];
    private static readonly string[] ToneLabels = ["Soft +2", "Soft +1", "Neutral", "Clear +1", "Clear +2"];

    public MainWindow()
    {
        InitializeComponent();
        SourceInitialized += (_, _) => EnableDarkTitleBar();

        _tray = CreateTrayIcon();
        _conn.ConnectionChanged += (ok, msg) => Dispatcher.Invoke(() => OnConnectionChanged(ok, msg));
        _conn.StateChanged += st => Dispatcher.Invoke(() => ApplyStateToUi(st));

        Loaded += async (_, _) => await ConnectAsync();
    }

    private async Task ConnectAsync()
    {
        ReconnectBtn.IsEnabled = false;
        StatusSubText.Text = "Connecting over Bluetooth RFCOMM...";
        ConnDot.Fill = new SolidColorBrush(System.Windows.Media.Color.FromRgb(0xF5, 0x9E, 0x0B));
        await _conn.ConnectAutoAsync();
        ReconnectBtn.IsEnabled = true;
    }

    private void OnConnectionChanged(bool connected, string msg)
    {
        ControlsRoot.IsEnabled = connected;
        DeviceNameText.Text = _conn.CurrentDevice?.Name ?? "Galaxy Buds";
        StatusSubText.Text = msg;
        ConnDot.Fill = new SolidColorBrush(connected
            ? System.Windows.Media.Color.FromRgb(0x10, 0xB9, 0x81)
            : System.Windows.Media.Color.FromRgb(0xEF, 0x44, 0x44));
        _tray.Text = connected ? $"{DeviceNameText.Text} — Connected" : "Pods4 — Disconnected";
    }

    private void ApplyStateToUi(BudsState s)
    {
        _syncing = true;
        try
        {
            // Batteries & wear state
            BatLeftText.Text = s.PlacementL == PlacementState.Disconnected ? "—" : $"{s.BatteryL}%";
            BatRightText.Text = s.PlacementR == PlacementState.Disconnected ? "—" : $"{s.BatteryR}%";
            BatCaseText.Text = s.BatteryCase is > 0 and <= 100 ? $"{s.BatteryCase}%" : "—";
            WearLeftText.Text = FormatPlacement(s.PlacementL);
            WearRightText.Text = FormatPlacement(s.PlacementR);
            CaseSubText.Text = s.IsCaseCharging ? "Charging" : (s.BatteryCase is > 0 and <= 100 ? "Ready" : "Closed");

            // Noise control mode
            ModeOffBtn.IsChecked = s.Mode == NoiseControlMode.Off;
            ModeAncBtn.IsChecked = s.Mode == NoiseControlMode.Anc;
            ModeAmbientBtn.IsChecked = s.Mode == NoiseControlMode.Ambient;
            ModeAdaptiveBtn.IsChecked = s.Mode == NoiseControlMode.Adaptive;

            bool oneBudWorn = (s.PlacementL == PlacementState.Wearing) ^ (s.PlacementR == PlacementState.Wearing);
            OneBudHintText.Visibility = oneBudWorn && !s.OneEarbudNoiseControl ? Visibility.Visible : Visibility.Collapsed;

            // Levels
            _lastAmbientVol = s.AmbientVolume;
            AmbientVolSlider.Value = s.AmbientVolume;
            AmbientVolLabel.Text = $"Level {s.AmbientVolume + 1}";

            _lastAncLevel = s.AncLevel;
            AncLevelSlider.Value = s.AncLevel;
            AncLevelLabel.Text = $"Level {s.AncLevel + 1}";

            // Custom Ambient
            CustomAmbientToggle.IsChecked = s.CustomAmbientOn;
            CustomAmbientPanel.IsEnabled = s.CustomAmbientOn;
            CustomAmbientPanel.Opacity = s.CustomAmbientOn ? 1.0 : 0.5;
            CustomLeftSlider.Value = s.CustomAmbientLeft;
            CustomRightSlider.Value = s.CustomAmbientRight;
            CustomToneSlider.Value = s.CustomAmbientTone;
            CustomLeftLabel.Text = BoostLabels[Math.Clamp(s.CustomAmbientLeft, 0, 2)];
            CustomRightLabel.Text = BoostLabels[Math.Clamp(s.CustomAmbientRight, 0, 2)];
            CustomToneLabel.Text = ToneLabels[Math.Clamp(s.CustomAmbientTone, 0, 4)];

            // Smart options
            VoiceDetectToggle.IsChecked = s.VoiceDetect;
            VoiceDurPanel.IsEnabled = s.VoiceDetect;
            Dur5Btn.IsChecked = s.VoiceDetectDuration == 0;
            Dur10Btn.IsChecked = s.VoiceDetectDuration == 1;
            Dur15Btn.IsChecked = s.VoiceDetectDuration == 2;

            SirenDetectToggle.IsChecked = s.SirenDetect;
            SidetoneToggle.IsChecked = s.AmbientDuringCalls;
            OneBudToggle.IsChecked = s.OneEarbudNoiseControl;

            UpdateTrayVisual(s.Mode);
        }
        finally
        {
            _syncing = false;
        }
    }

    private static string FormatPlacement(PlacementState p) => p switch
    {
        PlacementState.Wearing => "In Ear",
        PlacementState.Idle => "Out of Ear",
        PlacementState.Case => "In Case",
        PlacementState.ClosedCase => "Case Closed",
        PlacementState.Charging => "Charging",
        _ => "Offline"
    };

    private async void ReconnectBtn_Click(object sender, RoutedEventArgs e) => await ConnectAsync();

    private async void ModeBtn_Checked(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        var mode = sender == ModeAncBtn ? NoiseControlMode.Anc
            : sender == ModeAmbientBtn ? NoiseControlMode.Ambient
            : sender == ModeAdaptiveBtn ? NoiseControlMode.Adaptive
            : NoiseControlMode.Off;
        UpdateTrayVisual(mode);
        await _conn.SendAsync(BudsProtocol.SetNoiseControl(mode));
    }

    private async void AmbientVolSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        int val = (int)Math.Round(e.NewValue);
        if (AmbientVolLabel != null) AmbientVolLabel.Text = $"Level {val + 1}";
        if (_syncing || val == _lastAmbientVol) return;
        _lastAmbientVol = val;
        await _conn.SendAsync(BudsProtocol.SetAmbientVolume(val));
    }

    private async void AncLevelSlider_ValueChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        int val = (int)Math.Round(e.NewValue);
        if (AncLevelLabel != null) AncLevelLabel.Text = $"Level {val + 1}";
        if (_syncing || val == _lastAncLevel) return;
        _lastAncLevel = val;
        await _conn.SendAsync(BudsProtocol.SetAncLevel(val));
    }

    private async void CustomAmbient_Changed(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        bool on = CustomAmbientToggle.IsChecked == true;
        CustomAmbientPanel.IsEnabled = on;
        CustomAmbientPanel.Opacity = on ? 1.0 : 0.5;
        await SendCustomizeAmbientAsync();
    }

    private async void CustomAmbient_SliderChanged(object sender, RoutedPropertyChangedEventArgs<double> e)
    {
        if (!IsLoaded) return;
        CustomLeftLabel.Text = BoostLabels[Math.Clamp((int)Math.Round(CustomLeftSlider.Value), 0, 2)];
        CustomRightLabel.Text = BoostLabels[Math.Clamp((int)Math.Round(CustomRightSlider.Value), 0, 2)];
        CustomToneLabel.Text = ToneLabels[Math.Clamp((int)Math.Round(CustomToneSlider.Value), 0, 4)];
        if (_syncing) return;
        await SendCustomizeAmbientAsync();
    }

    private Task SendCustomizeAmbientAsync() =>
        _conn.SendAsync(BudsProtocol.SetCustomizeAmbient(
            CustomAmbientToggle.IsChecked == true,
            (int)Math.Round(CustomLeftSlider.Value),
            (int)Math.Round(CustomRightSlider.Value),
            (int)Math.Round(CustomToneSlider.Value)));

    private async void VoiceDetectToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        bool on = VoiceDetectToggle.IsChecked == true;
        VoiceDurPanel.IsEnabled = on;
        await _conn.SendAsync(BudsProtocol.SetVoiceDetect(on));
    }

    private async void VoiceDur_Checked(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        int idx = sender == Dur15Btn ? 2 : sender == Dur10Btn ? 1 : 0;
        await _conn.SendAsync(BudsProtocol.SetVoiceDetectDuration(idx));
    }

    private async void SirenDetectToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        await _conn.SendAsync(BudsProtocol.SetSirenDetect(SirenDetectToggle.IsChecked == true));
    }

    private async void SidetoneToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        await _conn.SendAsync(BudsProtocol.SetAmbientDuringCalls(SidetoneToggle.IsChecked == true));
    }

    private async void OneBudToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_syncing) return;
        bool on = OneBudToggle.IsChecked == true;
        if (on) OneBudHintText.Visibility = Visibility.Collapsed;
        await _conn.SendAsync(BudsProtocol.SetOneEarbudNoiseControl(on));
    }

    // --- System Tray ---
    private Forms.NotifyIcon CreateTrayIcon()
    {
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("Show Controls", null, (_, _) => ShowFromTray());
        menu.Items.Add(new Forms.ToolStripSeparator());
        menu.Items.Add("Ambient Sound (Passthrough)", null, async (_, _) => await _conn.SendAsync(BudsProtocol.SetNoiseControl(NoiseControlMode.Ambient)));
        menu.Items.Add("Active Noise Cancelling", null, async (_, _) => await _conn.SendAsync(BudsProtocol.SetNoiseControl(NoiseControlMode.Anc)));
        menu.Items.Add("Adaptive", null, async (_, _) => await _conn.SendAsync(BudsProtocol.SetNoiseControl(NoiseControlMode.Adaptive)));
        menu.Items.Add("Off", null, async (_, _) => await _conn.SendAsync(BudsProtocol.SetNoiseControl(NoiseControlMode.Off)));
        menu.Items.Add(new Forms.ToolStripSeparator());
        menu.Items.Add("Reconnect", null, async (_, _) => await Dispatcher.InvokeAsync(ConnectAsync));
        menu.Items.Add("Exit", null, (_, _) => { _exiting = true; Close(); });

        var icon = new Forms.NotifyIcon
        {
            Text = "Pods4 — Passthrough Controls",
            Icon = MakeDotIcon(System.Drawing.Color.FromArgb(0x64, 0x74, 0x8B)),
            ContextMenuStrip = menu,
            Visible = true
        };
        icon.Click += (_, e) =>
        {
            if (e is Forms.MouseEventArgs me && me.Button == Forms.MouseButtons.Left)
                Dispatcher.Invoke(() => { if (IsVisible) Hide(); else ShowFromTray(); });
        };
        return icon;
    }

    private void UpdateTrayVisual(NoiseControlMode mode)
    {
        var color = mode switch
        {
            NoiseControlMode.Ambient => System.Drawing.Color.FromArgb(0x10, 0xB9, 0x81),
            NoiseControlMode.Anc => System.Drawing.Color.FromArgb(0x3B, 0x82, 0xF6),
            NoiseControlMode.Adaptive => System.Drawing.Color.FromArgb(0x8B, 0x5C, 0xF6),
            _ => System.Drawing.Color.FromArgb(0x64, 0x74, 0x8B)
        };
        var prev = _tray.Icon;
        _tray.Icon = MakeDotIcon(color);
        prev?.Dispose();
        _tray.Text = $"{_conn.CurrentDevice?.Name ?? "Pods4"} — {mode}";
    }

    private static System.Drawing.Icon MakeDotIcon(System.Drawing.Color fill)
    {
        using var bmp = new Bitmap(32, 32);
        using (var g = Graphics.FromImage(bmp))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            using var bg = new SolidBrush(fill);
            g.FillEllipse(bg, 3, 3, 26, 26);
            using var ring = new System.Drawing.Pen(System.Drawing.Color.White, 2.2f);
            g.DrawEllipse(ring, 9, 9, 14, 14);
        }
        IntPtr hIcon = bmp.GetHicon();
        try { return (System.Drawing.Icon)System.Drawing.Icon.FromHandle(hIcon).Clone(); }
        finally { DestroyIcon(hIcon); }
    }

    public void ShowFromTray()
    {
        Show();
        WindowState = WindowState.Normal;
        Topmost = true;
        Topmost = false;
        Activate();
        if (!_conn.IsConnected && ReconnectBtn.IsEnabled)
            _ = ConnectAsync();
    }

    protected override void OnClosing(CancelEventArgs e)
    {
        if (!_exiting)
        {
            e.Cancel = true;
            Hide();
            return;
        }
        _tray.Visible = false;
        _tray.Dispose();
        _conn.Dispose();
        base.OnClosing(e);
    }

    private void EnableDarkTitleBar()
    {
        var hwnd = new WindowInteropHelper(this).Handle;
        int dark = 1;
        DwmSetWindowAttribute(hwnd, 20, ref dark, sizeof(int));
    }

    [DllImport("dwmapi.dll")] private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int val, int size);
    [DllImport("user32.dll")] private static extern bool DestroyIcon(IntPtr hIcon);
}
