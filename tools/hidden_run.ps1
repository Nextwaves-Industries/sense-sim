# Runs a test copy of the app on its own hidden Windows desktop.
#
# A test window on the normal desktop catches whatever the person at this computer is
# clicking, and those clicks are lost when it closes. On a hidden desktop nothing shows
# and only `--drive` input reaches it, so scripted runs come out the same every time.
#
#   tools\hidden_run.ps1 -Exe <test build>\SenseSim.exe -Seconds 60 -- --open antenna-draw --shot-delay 7 --screenshot C:\temp\draw.png
#
# Stops the copy after -Seconds if it hasn't exited (a `--screenshot` run exits by itself).
# -NoWait returns at once with its process id, for a copy to talk to over `--mcp-port`.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$Seconds = 120,
    [switch]$NoWait,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)
if (-not ("SenseSimHiddenDesktop" -as [type])) {
    Add-Type @"
using System; using System.Runtime.InteropServices;
public static class SenseSimHiddenDesktop {
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct STARTUPINFO { public int cb; public string lpReserved; public string lpDesktop; public string lpTitle; public int dwX,dwY,dwXSize,dwYSize,dwXCountChars,dwYCountChars,dwFillAttribute,dwFlags; public short wShowWindow,cbReserved2; public IntPtr lpReserved2,hStdInput,hStdOutput,hStdError; }
 [StructLayout(LayoutKind.Sequential)] public struct PROCESS_INFORMATION { public IntPtr hProcess,hThread; public int dwProcessId,dwThreadId; }
 [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern IntPtr CreateDesktopW(string d,IntPtr a,IntPtr b,int f,uint acc,IntPtr sa);
 [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool CreateProcessW(string app,string cmd,IntPtr pa,IntPtr ta,bool inh,uint flags,IntPtr env,string cwd,ref STARTUPINFO si,out PROCESS_INFORMATION pi);
 [DllImport("kernel32.dll")] public static extern uint WaitForSingleObject(IntPtr h,uint ms);
 [DllImport("kernel32.dll")] public static extern bool TerminateProcess(IntPtr h,uint c);
}
"@
}
$name = "sensesim_hidden_test"
$desk = [SenseSimHiddenDesktop]::CreateDesktopW($name, [IntPtr]::Zero, [IntPtr]::Zero, 0, 0x10000000, [IntPtr]::Zero)
if ($desk -eq [IntPtr]::Zero) { throw "could not create the hidden desktop" }
$si = New-Object SenseSimHiddenDesktop+STARTUPINFO
$si.cb = [Runtime.InteropServices.Marshal]::SizeOf([type][SenseSimHiddenDesktop+STARTUPINFO])
$si.lpDesktop = $name
$pi = New-Object SenseSimHiddenDesktop+PROCESS_INFORMATION
$cmd = '"' + $Exe + '" ' + (($Rest | Where-Object { $_ -ne '--' } | ForEach-Object { '"' + $_ + '"' }) -join ' ')
# CREATE_NO_WINDOW: no console flashing up on the person's desktop.
if (-not [SenseSimHiddenDesktop]::CreateProcessW($Exe, $cmd, [IntPtr]::Zero, [IntPtr]::Zero, $false, 0x08000000, [IntPtr]::Zero, (Get-Location).Path, [ref]$si, [ref]$pi)) { throw "could not start $Exe" }
if ($NoWait) { Write-Output $pi.dwProcessId; exit 0 }
if ([SenseSimHiddenDesktop]::WaitForSingleObject($pi.hProcess, [uint32]($Seconds * 1000)) -ne 0) {
    [SenseSimHiddenDesktop]::TerminateProcess($pi.hProcess, 1) | Out-Null
    Write-Output "stopped pid $($pi.dwProcessId) after $Seconds s"
}
