// Synthetic app for installer tests. Never reads SightOCR user settings or models.
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public sealed class ShutdownFixture : Form
{
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern bool SetProp(IntPtr window, string name, IntPtr value);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern uint RegisterWindowMessage(string name);
    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")]
    static extern bool EnumWindows(EnumWindow callback, IntPtr parameter);
    delegate bool EnumWindow(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")]
    static extern IntPtr GetDlgItem(IntPtr dialog, int item);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int size);

    readonly uint exitMessage = RegisterWindowMessage("SightOCR.MainWindow.Exit");
    readonly string directory = AppDomain.CurrentDomain.BaseDirectory;

    [STAThread]
    static void Main()
    {
        Application.Run(new ShutdownFixture());
    }

    public ShutdownFixture()
    {
        Text = "SightOCR Installer Synthetic Fixture";
        ShowInTaskbar = false;
        var window = Handle;
        if (!File.Exists(Path.Combine(directory, "legacy")))
            SetProp(window, "SightOCR.MainWindow.ExitSupported", new IntPtr(1));
        File.AppendAllText(Path.Combine(directory, "events.log"), "started:" + Process.GetCurrentProcess().Id + "\n");
        File.WriteAllText(Path.Combine(directory, "window.txt"), window.ToInt64().ToString());
    }

    protected override void SetVisibleCore(bool value) { base.SetVisibleCore(false); }
    protected override void WndProc(ref Message message)
    {
        if (message.Msg == exitMessage)
        {
            File.AppendAllText(Path.Combine(directory, "events.log"), "graceful:" + Process.GetCurrentProcess().Id + "\n");
            Application.Exit();
            return;
        }
        base.WndProc(ref message);
    }

    // Only the explicitly provided installer process can own a clickable dialog.
    public static bool RespondToInstaller(uint processId, int button)
    {
        bool found = false;
        EnumWindows(delegate(IntPtr window, IntPtr parameter)
        {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != processId) return true;
            var child = GetDlgItem(window, 65535); // Standard MessageBox static text.
            var text = new System.Text.StringBuilder(1024);
            GetWindowText(child, text, text.Capacity);
            if (!text.ToString().Contains("SightOCR 当前正在运行")) return true;
            var control = GetDlgItem(window, button);
            if (control == IntPtr.Zero) return true;
            PostMessage(control, 0x00F5, IntPtr.Zero, IntPtr.Zero); // BM_CLICK
            found = true;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    public static void RequestShutdown(uint processId)
    {
        EnumWindows(delegate(IntPtr window, IntPtr parameter)
        {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner == processId)
                PostMessage(window, RegisterWindowMessage("SightOCR.MainWindow.Exit"), IntPtr.Zero, IntPtr.Zero);
            return true;
        }, IntPtr.Zero);
    }
}
