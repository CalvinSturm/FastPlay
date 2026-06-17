Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Drawing;
using System.Drawing.Imaging;
public class Win32 {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    public static IntPtr Found = IntPtr.Zero;
    public static IntPtr FindByTitle(string needle) {
        Found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr l) {
            if (!IsWindowVisible(h)) return true;
            var sb = new System.Text.StringBuilder(256);
            GetWindowText(h, sb, 256);
            if (sb.ToString().Contains(needle)) { Found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return Found;
    }
}
"@ -ReferencedAssemblies System.Drawing
Add-Type -AssemblyName System.Drawing

$LEFTDOWN = 0x0002; $LEFTUP = 0x0004

function Get-Win { return [Win32]::FindByTitle("FastPlay") }

function Get-ClientScreenRect($h) {
    $r = New-Object Win32+RECT
    [void][Win32]::GetClientRect($h, [ref]$r)
    $tl = New-Object Win32+POINT; $tl.X = 0; $tl.Y = 0
    [void][Win32]::ClientToScreen($h, [ref]$tl)
    return @{ X = $tl.X; Y = $tl.Y; W = $r.Right - $r.Left; H = $r.Bottom - $r.Top }
}

function Save-Shot($rect, $path) {
    $bmp = New-Object System.Drawing.Bitmap($rect.W, $rect.H)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.X, $rect.Y, 0, 0, (New-Object System.Drawing.Size($rect.W, $rect.H)))
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

# Mean abs pixel diff between two PNGs (sampled) -> rough motion metric
function Frame-Diff($p1, $p2) {
    $b1 = New-Object System.Drawing.Bitmap($p1)
    $b2 = New-Object System.Drawing.Bitmap($p2)
    $w = [Math]::Min($b1.Width, $b2.Width); $h = [Math]::Min($b1.Height, $b2.Height)
    $sum = 0.0; $n = 0
    for ($y = 0; $y -lt $h; $y += 16) {
        for ($x = 0; $x -lt $w; $x += 16) {
            $c1 = $b1.GetPixel($x, $y); $c2 = $b2.GetPixel($x, $y)
            $sum += [Math]::Abs($c1.R - $c2.R) + [Math]::Abs($c1.G - $c2.G) + [Math]::Abs($c1.B - $c2.B)
            $n++
        }
    }
    $b1.Dispose(); $b2.Dispose()
    return [Math]::Round($sum / $n, 2)
}
