# Run only3d's screenshot self-test in the logged-on desktop session (a program started over
# SSH has no desktop). vm.sh push this file, then:
#   vm.sh ssh <name> 'C:\vmtest\shot.ps1 -Out C:\vmtest\prefs.png -Open prefs'
param(
  [string]$Out = 'C:\vmtest\shot.png',
  [string]$Open = '',
  [string]$Project = 'C:\vmtest\japandi_tray.only3d.json',
  [string]$View = ''
)
$exe = "$env:LOCALAPPDATA\Programs\only3d\only3d.exe"
$log = [IO.Path]::ChangeExtension($Out, '.log')
Remove-Item $Out, $log -ErrorAction SilentlyContinue
$envs = "set ONLY3D_HOME=C:\vmtest\shothome&& set ONLY3D_GUI_STATS=1&& set ONLY3D_GUI_SHOT_DELAY_MS=4000"
if ($Open) { $envs += "&& set ONLY3D_GUI_OPEN=$Open" }
if ($View) { $envs += "&& set ONLY3D_GUI_VIEW=$View" }
$line = "$envs&& `"$exe`" gui `"$Project`" --no-live --screenshot `"$Out`" > `"$log`" 2>&1"
$action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument "/c $line"
$who = New-ScheduledTaskPrincipal -UserId "$env:COMPUTERNAME\Administrator" -LogonType Interactive -RunLevel Highest
Register-ScheduledTask -TaskName only3d-shot -Action $action -Principal $who -Force | Out-Null
Start-ScheduledTask -TaskName only3d-shot
for ($i = 0; $i -lt 90 -and -not (Test-Path $Out); $i++) { Start-Sleep 1 }
Start-Sleep 1
Unregister-ScheduledTask -TaskName only3d-shot -Confirm:$false
if (Test-Path $Out) { "saved $Out" } else { "no screenshot after 90 s" }
if (Test-Path $log) { Get-Content $log }
