// Isolated updater fixture. Never uses SightOCR settings, models or registry.
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public static class UpdateFixture
{
    [DllImport("user32.dll")]
    static extern bool EnumWindows(EnumWindow callback, IntPtr parameter);
    [DllImport("user32.dll")]
    static extern bool EnumChildWindows(IntPtr parent, EnumWindow callback, IntPtr parameter);
    delegate bool EnumWindow(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")]
    static extern IntPtr GetDlgItem(IntPtr dialog, int item);
    [DllImport("user32.dll")]
    static extern int GetDlgCtrlID(IntPtr control);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern int GetWindowText(IntPtr window, StringBuilder text, int size);
    [DllImport("user32.dll")]
    static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr SendMessageTimeout(IntPtr window, uint message, IntPtr wparam,
        StringBuilder text, uint flags, uint timeout, out IntPtr result);

    static int Main(string[] arguments)
    {
        string directory = AppDomain.CurrentDomain.BaseDirectory;
        try
        {
            if (String.Equals(Path.GetFileNameWithoutExtension(Process.GetCurrentProcess().MainModule.FileName),
                "SightOCR", StringComparison.OrdinalIgnoreCase))
            {
                File.AppendAllText(Path.Combine(directory, "app-events.log"),
                    "started:" + String.Join("|", arguments) + "\n", Encoding.UTF8);
                if (Array.IndexOf(arguments, "--hold-for-helper") >= 0)
                {
                    var timer = Stopwatch.StartNew();
                    while (!File.Exists(Path.Combine(directory, "release-parent")) && timer.ElapsedMilliseconds < 20000)
                        System.Threading.Thread.Sleep(25);
                    return timer.ElapsedMilliseconds < 20000 ? 0 : 93;
                }
                return 0;
            }

            File.WriteAllLines(Path.Combine(directory, "setup-arguments.txt"), arguments, Encoding.UTF8);
            File.AppendAllText(Path.Combine(directory, "setup-events.log"), "started\n", Encoding.UTF8);
            string installDirectory = null;
            foreach (string argument in arguments)
                if (argument.StartsWith("/DIR=", StringComparison.OrdinalIgnoreCase))
                    installDirectory = argument.Substring(5);
            if (installDirectory == null || !Directory.Exists(installDirectory)) return 90;
            if (File.ReadAllText(Path.Combine(directory, "fixture-mode.txt")).Trim() == "failure") return 42;

            // Inno Setup owns the successful restart. The helper must not add a second.
            var start = new ProcessStartInfo(Path.Combine(installDirectory, "SightOCR.exe"), "--from-setup");
            start.UseShellExecute = false;
            start.CreateNoWindow = true;
            using (var app = Process.Start(start))
            {
                if (!app.WaitForExit(10000)) return 91;
                if (app.ExitCode != 0) return 92;
            }
            return 0;
        }
        catch (Exception error)
        {
            File.WriteAllText(Path.Combine(directory, "fixture-error.txt"), error.ToString(), Encoding.UTF8);
            return 99;
        }
    }

    // Only dismiss the exact helper PID's own standard update error MessageBox.
    // No global title matching, unrelated controls or user application windows.
    public static string DismissOwnUpdateError(uint processId, string diagnosticPath)
    {
        string found = null;
        var diagnostic = new StringBuilder();
        EnumWindows(delegate(IntPtr window, IntPtr parameter)
        {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != processId) return true;
            string message = null;
            var title = new StringBuilder(256);
            GetWindowText(window, title, title.Capacity);
            diagnostic.AppendLine("Window: " + title.ToString());
            // Icon and text Static controls can share ID 65535, with order
            // depending on layout. Inspect only this owned dialog's children.
            EnumChildWindows(window, delegate(IntPtr child, IntPtr unused)
            {
                var text = new StringBuilder(2048);
                IntPtr length;
                SendMessageTimeout(child, 0x000D, new IntPtr(text.Capacity), text, 2, 200, out length);
                diagnostic.AppendLine("Control " + GetDlgCtrlID(child) + ": " + text.ToString());
                if (!text.ToString().StartsWith("自动更新未完成：", StringComparison.Ordinal)) return true;
                message = text.ToString();
                return false;
            }, IntPtr.Zero);
            diagnostic.AppendLine("Matched: " + (message != null).ToString());
            if (message == null) return true;
            var okay = GetDlgItem(window, 1);
            // Some Windows builds give the sole MB_OK acknowledgement ID 2.
            // This is still the exact owned update error, not an install prompt.
            if (okay == IntPtr.Zero) okay = GetDlgItem(window, 2);
            diagnostic.AppendLine("OK handle: " + okay.ToInt64());
            if (okay == IntPtr.Zero) return true;
            found = message;
            PostMessage(okay, 0x00F5, IntPtr.Zero, IntPtr.Zero); // BM_CLICK on owned acknowledgement only.
            return false;
        }, IntPtr.Zero);
        if (diagnostic.Length > 0) File.WriteAllText(diagnosticPath, diagnostic.ToString(), Encoding.UTF8);
        return found;
    }
}
