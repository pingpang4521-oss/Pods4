using System.Windows;

namespace Pods4;

public partial class App : System.Windows.Application
{
    private const string MutexName = @"Local\Pods4_SingleInstance";
    private const string ShowEventName = @"Local\Pods4_ShowWindow";

    private Mutex? _instanceMutex;
    private EventWaitHandle? _showEvent;
    private CancellationTokenSource? _signalCts;

    protected override void OnStartup(StartupEventArgs e)
    {
        if (e.Args.Contains("--self-check"))
        {
            BudsProtocol.RunSelfCheck();
            Shutdown(0);
            return;
        }

        _instanceMutex = new Mutex(true, MutexName, out bool isFirstInstance);
        _showEvent = new EventWaitHandle(false, EventResetMode.AutoReset, ShowEventName);

        if (!isFirstInstance)
        {
            _showEvent.Set();
            _showEvent.Dispose();
            _instanceMutex.Dispose();
            Shutdown(0);
            return;
        }

        base.OnStartup(e);
        var win = new MainWindow();
        MainWindow = win;
        win.Show();

        _signalCts = new CancellationTokenSource();
        var token = _signalCts.Token;
        Task.Run(() =>
        {
            var handles = new WaitHandle[] { _showEvent, token.WaitHandle };
            while (WaitHandle.WaitAny(handles) == 0)
            {
                Dispatcher.Invoke(win.ShowFromTray);
            }
        }, token);
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _signalCts?.Cancel();
        _signalCts?.Dispose();
        _showEvent?.Dispose();
        if (_instanceMutex != null)
        {
            try { _instanceMutex.ReleaseMutex(); } catch { }
            _instanceMutex.Dispose();
        }
        base.OnExit(e);
    }
}
