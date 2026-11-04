# UIA lookups for the Avalonia build. Visible elements only; interactive types first.
function All-Elements([IntPtr]$h) {
  # A tree being replaced (for example during Open) can briefly fail the COM walk.
  for ($attempt = 1; ; $attempt++) {
    try {
      $root = [System.Windows.Automation.AutomationElement]::FromHandle($h)
      return $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    } catch {
      if ($attempt -ge 4) { throw }
      Start-Sleep -Milliseconds 400
    }
  }
}
function Visible($e) {
  $r = $e.Current.BoundingRectangle
  -not $r.IsEmpty -and -not [double]::IsInfinity($r.Width) -and -not [double]::IsInfinity($r.X) -and $r.Width -gt 0 -and $r.Height -gt 0
}
function Find-Element([IntPtr]$h, [string]$prefix, [int]$nth = 0) {
  $interactive = "ControlType.Button","ControlType.ListItem","ControlType.TabItem","ControlType.Edit","ControlType.Slider"
  $found = @(All-Elements $h | Where-Object { ($_.Current.Name -eq $prefix -or $_.Current.Name.StartsWith("$prefix,")) -and (Visible $_) } |
    Sort-Object { if ($interactive -contains $_.Current.ControlType.ProgrammaticName) { 0 } else { 1 } })
  if ($found.Count -le $nth) { throw "element '$prefix' not found" }
  $found[$nth]
}
function Click-Named([IntPtr]$h, [string]$name, [int]$nth = 0) {
  Assert-CutCapForeground
  $r = (Find-Element $h $name $nth).Current.BoundingRectangle
  Focus-CutCap $h
  [void][Native2]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)); Start-Sleep -Milliseconds 80
  [Native2]::mouse_event(2, 0, 0, 0, [IntPtr]::Zero); Start-Sleep -Milliseconds 40; [Native2]::mouse_event(4, 0, 0, 0, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 300
}
function Selected-Names([IntPtr]$h, [string]$listName) {
  $list = Find-Element $h $listName
  $items = $list.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
  ($items | Where-Object {
      $pattern = $null
      $_.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern) -and $pattern.Current.IsSelected
    } | ForEach-Object { $_.Current.Name }) -join '; '
}
# Text of the first visible text element containing $fragment (optionally an exact accessible name).
function Read-Text([IntPtr]$h, [string]$fragment, [string]$name = '') {
  if ($name) {
    $e = Find-Element $h $name
    $value = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$value)) { return $value.Current.Value }
    return $e.Current.Name
  }
  (All-Elements $h | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Text' -and $_.Current.Name.Contains($fragment) -and (Visible $_) } |
    Select-Object -First 1).Current.Name
}
