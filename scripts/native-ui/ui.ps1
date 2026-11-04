# Native UI test harness for CutCap (dot-source this file).
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
if (-not ('Native2' -as [type])) {
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Native2 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int d, IntPtr e);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr e);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
}
"@
}
[void][Native2]::SetThreadDpiAwarenessContext([IntPtr](-4))

# Safety: refuse to send any synthetic input unless CutCap (or its own dialog) is foreground.
$script:CutCapPid = 0
function Assert-CutCapForeground {
  $fg = [Native2]::GetForegroundWindow(); [uint32]$owner = 0
  [void][Native2]::GetWindowThreadProcessId($fg, [ref]$owner)
  if ($script:CutCapPid -eq 0 -or $owner -ne $script:CutCapPid) { throw "ABORT: foreground window belongs to pid $owner, not CutCap ($script:CutCapPid). No input sent." }
}
function Focus-CutCap([IntPtr]$h) {
  [void][Native2]::SetForegroundWindow($h); Start-Sleep -Milliseconds 150; Assert-CutCapForeground
}
if (-not ('WinEnum' -as [type])) {
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class WinEnum {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  public static IntPtr FindTitled(uint pid, string suffix) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != pid || !IsWindowVisible(h)) return true;
      var sb = new StringBuilder(512); GetWindowText(h, sb, 512);
      if (sb.ToString().EndsWith(suffix)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@
}
# The main window is the visible one titled "<project> — CutCap"; popups
# (tooltips, menus) are separate top-level windows of the same process.
function Get-CutCapWindow([int]$ProcessId) {
  for ($i = 0; $i -lt 100; $i++) {
    $h = [WinEnum]::FindTitled([uint32]$ProcessId, [string][char]0x2014 + ' CutCap')
    if ($h -ne [IntPtr]::Zero) { return $h }
    Start-Sleep -Milliseconds 100
  }
  throw "window not found"
}
function Get-ClientOrigin([IntPtr]$h) { $p = New-Object Native2+POINT; [void][Native2]::ClientToScreen($h, [ref]$p); $p }
function Get-ClientSize([IntPtr]$h) { $r = New-Object Native2+RECT; [void][Native2]::GetClientRect($h, [ref]$r); @{ W = $r.Right; H = $r.Bottom } }
function Save-Client([IntPtr]$h, [string]$path) {
  Focus-CutCap $h; Start-Sleep -Milliseconds 250
  $o = Get-ClientOrigin $h; $s = Get-ClientSize $h
  $wr = New-Object Native2+RECT; [void][Native2]::GetWindowRect($h, [ref]$wr)
  $s.W = [Math]::Min($s.W, $wr.Right - $o.X); $s.H = [Math]::Min($s.H, $wr.Bottom - $o.Y)
  $bmp = New-Object System.Drawing.Bitmap $s.W, $s.H
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($o.X, $o.Y, 0, 0, (New-Object System.Drawing.Size $s.W, $s.H))
  $g.Dispose()
  $codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq 'image/jpeg'
  $ep = New-Object System.Drawing.Imaging.EncoderParameters 1
  $ep.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 88L
  $bmp.Save($path, $codec, $ep); $bmp.Dispose()
  "$($s.W)x$($s.H) -> $path"
}
# Logical (Slint) coordinates; scale converts to physical client pixels.
$script:Scale = 1.5
function Click-At([IntPtr]$h, [double]$lx, [double]$ly, [switch]$Double) {
  Focus-CutCap $h
  $o = Get-ClientOrigin $h
  [void][Native2]::SetCursorPos([int]($o.X + $lx * $script:Scale), [int]($o.Y + $ly * $script:Scale))
  Start-Sleep -Milliseconds 60
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  if ($Double) { Start-Sleep -Milliseconds 60; [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero) }
  Start-Sleep -Milliseconds 150
}
function Drag-From([IntPtr]$h, [double]$x1, [double]$y1, [double]$x2, [double]$y2, [int]$steps = 12) {
  Focus-CutCap $h
  $o = Get-ClientOrigin $h
  [void][Native2]::SetCursorPos([int]($o.X + $x1 * $script:Scale), [int]($o.Y + $y1 * $script:Scale)); Start-Sleep -Milliseconds 60
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 60
  for ($i = 1; $i -le $steps; $i++) {
    $x = $x1 + ($x2 - $x1) * $i / $steps; $y = $y1 + ($y2 - $y1) * $i / $steps
    [void][Native2]::SetCursorPos([int]($o.X + $x * $script:Scale), [int]($o.Y + $y * $script:Scale)); Start-Sleep -Milliseconds 25
  }
  [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 150
}
function Hover-At([IntPtr]$h, [double]$lx, [double]$ly) {
  $o = Get-ClientOrigin $h
  [void][Native2]::SetCursorPos([int]($o.X + $lx * $script:Scale), [int]($o.Y + $ly * $script:Scale))
}
$script:VK = @{ CTRL = 0x11; SHIFT = 0x10; ALT = 0x12; TAB = 0x09; ENTER = 0x0D; SPACE = 0x20; ESC = 0x1B; LEFT = 0x25; UP = 0x26; RIGHT = 0x27; DOWN = 0x28; HOME = 0x24; END = 0x23; BACK = 0x08; DEL = 0x2E; MINUS = 0xBD; PLUS = 0xBB; PGUP = 0x21; PGDN = 0x22 }
function Send-Key([IntPtr]$h, [string]$combo) {
  Focus-CutCap $h
  $parts = $combo.Split('+')
  $codes = foreach ($p in $parts) { if ($script:VK.ContainsKey($p.ToUpper())) { $script:VK[$p.ToUpper()] } elseif ($p.Length -eq 1) { [byte][char]$p.ToUpper() } else { throw "key $p" } }
  foreach ($c in $codes) { [Native2]::keybd_event([byte]$c, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 20 }
  [array]::Reverse($codes)
  foreach ($c in $codes) { [Native2]::keybd_event([byte]$c, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 20 }
  Start-Sleep -Milliseconds 120
}
function Type-Text([IntPtr]$h, [string]$text) {
  foreach ($ch in $text.ToCharArray()) { Send-Key $h ([string]$ch) }
}
function Get-UiaTree([IntPtr]$h) {
  $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
  $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
  foreach ($e in $all) {
    $c = $e.Current
    [pscustomobject]@{ Type = $c.ControlType.ProgrammaticName -replace 'ControlType\.', ''; Name = $c.Name; Enabled = $c.IsEnabled; Focus = $c.HasKeyboardFocus; Help = $c.HelpText; Rect = "$($c.BoundingRectangle)" }
  }
}
function Get-Focused([IntPtr]$h) {
  $f = [System.Windows.Automation.AutomationElement]::FocusedElement
  if ($f) { "$($f.Current.ControlType.ProgrammaticName) '$($f.Current.Name)'" }
}
function Press([string]$combo) {
  Assert-CutCapForeground
  $codes = foreach ($p in $combo.Split('+')) { if ($script:VK.ContainsKey($p.ToUpper())) { $script:VK[$p.ToUpper()] } else { [byte][char]$p.ToUpper() } }
  foreach ($k in $codes) { [Native2]::keybd_event([byte]$k, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 20 }
  [array]::Reverse($codes); foreach ($k in $codes) { [Native2]::keybd_event([byte]$k, 0, 2, [IntPtr]::Zero); Start-Sleep -Milliseconds 20 }
  Start-Sleep -Milliseconds 120
}
function Complete-FileDialog([int]$ProcessId, [string]$path) {
  Add-Type -AssemblyName System.Windows.Forms
  $A = [System.Windows.Automation.AutomationElement]
  $main = $A::FromHandle((Get-CutCapWindow $ProcessId))
  $dlg = $null
  for ($i = 0; $i -lt 40 -and -not $dlg; $i++) {
    $dlg = $main.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -eq 'Import source video' } | Select-Object -First 1
    if (-not $dlg) { Start-Sleep -Milliseconds 150 }
  }
  if (-not $dlg) { throw "ABORT: import dialog not found; no input sent." }
  Assert-CutCapForeground
  $r = $dlg.Current.BoundingRectangle
  # File name box sits ~ (r.X + 0.45*W, r.Bottom - 92px physical).
  [void][Native2]::SetCursorPos([int]($r.X + $r.Width * 0.45), [int]($r.Bottom - 92)); Start-Sleep -Milliseconds 80
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 200
  Assert-CutCapForeground
  $escaped = ($path -replace '([+^%~(){}\[\]])', '{$1}')
  [System.Windows.Forms.SendKeys]::SendWait("^a$escaped{ENTER}")
}
function Import-Fixture([IntPtr]$h, [int]$ProcessId, [string]$path) {
  Send-Key $h "CTRL+I"; Start-Sleep -Milliseconds 800
  Complete-FileDialog $ProcessId $path; Start-Sleep -Milliseconds 2500
}






