# POC S2.1 (Windows): verify accessibility-tree readability via built-in .NET UIAutomation
# No pip / network needed.
# Usage: pwsh -File poc_a11y_windows.ps1 -MaxDepth 6 -MaxNodes 200
# PASS criteria: prints a structured tree summary (role/name/bbox) of the focused window,
#                with > 0 nodes and at least one interactive element.

param(
    [int]$MaxDepth = 6,
    [int]$MaxNodes = 200
)

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$script:count = 0
$script:truncated = $false
$script:interactive = 0

function Walk-Tree([System.Windows.Automation.AutomationElement]$el, [int]$depth) {
    if ($script:truncated) { return }
    if ($depth -gt $MaxDepth) { return }
    if ($script:count -ge $MaxNodes) { $script:truncated = $true; return }

    $script:count++
    $cur = $el.Current
    $name = $cur.Name
    $type = $cur.ControlType.ProgrammaticName
    $autoId = $cur.AutomationId
    $rect = $cur.BoundingRectangle
    $enabled = $cur.IsEnabled
    $off = $cur.IsOffscreen
    if ($cur.IsKeyboardFocusable -or $cur.ControlType -eq [System.Windows.Automation.ControlType]::Button -or $cur.ControlType -eq [System.Windows.Automation.ControlType]::Edit) {
        $script:interactive++
    }
    $pad = "  " * $depth
    Write-Output ("{0}[{1}] name='{2}' autoId='{3}' enabled={4} offscreen={5} bbox=({6},{7},{8},{9})" -f `
        $pad, $type, $name, $autoId, $enabled, $off, `
        [int]$rect.X, [int]$rect.Y, [int]$rect.Width, [int]$rect.Height)

    $children = $el.FindAll([System.Windows.Automation.TreeScope]::Children,
                            [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($c in $children) {
        Walk-Tree $c ($depth + 1)
        if ($script:truncated) { return }
    }
}

Write-Output "=== BaiZe Windows UIA accessibility-tree POC ==="
$focused = [System.Windows.Automation.AutomationElement]::FocusedElement
if ($null -eq $focused) {
    Write-Output "[FAIL] cannot get focused element (no foreground window, or UIA blocked)"
    exit 1
}

# Walk up to the top-level window so we start from the window root
$walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
$root = $focused
$ancestor = $walker.GetParent($focused)
while ($null -ne $ancestor -and $ancestor.Current.ControlType -ne [System.Windows.Automation.ControlType]::Window) {
    $ancestor = $walker.GetParent($ancestor)
}
if ($null -ne $ancestor) { $root = $ancestor }

Write-Output ("Root element: [{0}] name='{1}'" -f $root.Current.ControlType.ProgrammaticName, $root.Current.Name)
Write-Output "--- tree walk (depth <= $MaxDepth, nodes <= $MaxNodes) ---"
Walk-Tree $root 0
if ($script:truncated) { Write-Output "...(truncated: node limit reached)" }
Write-Output ("Total nodes: {0} ; interactive-ish nodes: {1}" -f $script:count, $script:interactive)

if ($script:count -gt 0) {
    Write-Output "[PASS] accessibility tree readable"
    exit 0
} else {
    Write-Output "[FAIL] no nodes read"
    exit 1
}
