<powershell>
# only3d test VM setup: Windows Server 2025 (same base as Windows 11 24H2). vm.sh fills in the
# __PLACEHOLDERS__ and passes this as EC2 user data. Log: C:\vmtest\setup.log
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
New-Item -ItemType Directory -Force C:\vmtest | Out-Null
Start-Transcript C:\vmtest\setup.log -Append
$deadline = __DEADLINE__
$gpu = '__GPU__'

# 1. Power off at the deadline (EC2 then deletes the VM), checked every 5 minutes and at boot
Set-Content C:\vmtest\deadline.txt $deadline
Set-Content C:\vmtest\deadline.ps1 'if ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() -ge [int64](Get-Content C:\vmtest\deadline.txt)) { Stop-Computer -Force }'
$act = New-ScheduledTaskAction -Execute powershell.exe -Argument '-NoProfile -ExecutionPolicy Bypass -File C:\vmtest\deadline.ps1'
$every = New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Days 2)
Register-ScheduledTask -TaskName only3d-deadline -Action $act -Trigger @($every, (New-ScheduledTaskTrigger -AtStartup)) -User SYSTEM -RunLevel Highest -Force | Out-Null

# 2. SSH with the tester's key, PowerShell as the shell
if (-not (Get-Service sshd -ErrorAction SilentlyContinue)) { Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0 | Out-Null }
Set-Service sshd -StartupType Automatic
Start-Service sshd
Set-Content -Path C:\ProgramData\ssh\administrators_authorized_keys -Value '__PUBKEY__'
icacls C:\ProgramData\ssh\administrators_authorized_keys /inheritance:r /grant 'Administrators:F' /grant 'SYSTEM:F' | Out-Null
New-ItemProperty -Path HKLM:\SOFTWARE\OpenSSH -Name DefaultShell -Value 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' -PropertyType String -Force | Out-Null
# the built-in rule only covers Private networks, and EC2's network counts as Public
if (Get-NetFirewallRule -Name OpenSSH-Server-In-TCP -ErrorAction SilentlyContinue) {
  Set-NetFirewallRule -Name OpenSSH-Server-In-TCP -Profile Any -Enabled True
} else {
  New-NetFirewallRule -Name OpenSSH-Server-In-TCP -DisplayName 'OpenSSH Server' -Protocol TCP -LocalPort 22 -Action Allow -Direction Inbound -Profile Any | Out-Null
}
Restart-Service sshd

# 3. Log on by itself, so DCV shows a desktop. The password never leaves the VM.
$chars = [char[]]'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789'
$pw = (-join (1..24 | ForEach-Object { $chars | Get-Random })) + 'a7#'
net user Administrator $pw | Out-Null
$wl = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty $wl AutoAdminLogon '1'
Set-ItemProperty $wl DefaultUserName 'Administrator'
Set-ItemProperty $wl DefaultPassword $pw
Set-ItemProperty $wl DefaultDomainName $env:COMPUTERNAME
powercfg /change monitor-timeout-ac 0
powercfg /change standby-timeout-ac 0
reg add 'HKLM\SOFTWARE\Policies\Microsoft\Windows\Personalization' /v NoLockScreen /t REG_DWORD /d 1 /f | Out-Null
Get-ScheduledTask -TaskName ServerManager -ErrorAction SilentlyContinue | Disable-ScheduledTask | Out-Null

# 4. Amazon DCV (free on EC2), with no login of its own: it's only reachable through the SSH tunnel
Invoke-WebRequest -UseBasicParsing https://d1uj6qtbmh3dt5.cloudfront.net/nice-dcv-server-x64-Release.msi -OutFile C:\vmtest\dcv-server.msi
Start-Process msiexec.exe -Wait -ArgumentList '/i C:\vmtest\dcv-server.msi ADDLOCAL=ALL /quiet /norestart /l*v C:\vmtest\dcv-install.log'
$dcv = 'Registry::HKEY_USERS\S-1-5-18\Software\GSettings\com\nicesoftware\dcv'
New-Item -Force "$dcv\security" | Out-Null
New-ItemProperty -Force -Path "$dcv\security" -Name authentication -Value none -PropertyType String | Out-Null
New-Item -Force "$dcv\session-management\automatic-console-session" | Out-Null
New-ItemProperty -Force -Path "$dcv\session-management\automatic-console-session" -Name owner -Value Administrator -PropertyType String | Out-Null
# the tester's own certificate, the same on every VM, so a browser accepts it once
$certDir = 'C:\Windows\System32\config\systemprofile\AppData\Local\NICE\dcv'
New-Item -ItemType Directory -Force $certDir | Out-Null
[IO.File]::WriteAllBytes("$certDir\dcv.pem", [Convert]::FromBase64String('__DCV_CERT__'))
[IO.File]::WriteAllBytes("$certDir\dcv.key", [Convert]::FromBase64String('__DCV_KEY__'))
$perm = 'C:\Program Files\NICE\DCV\Server\conf\default.perm'
if (Test-Path $perm) { Add-Content $perm "`r`n%any% allow builtin" }

# 5. The GPU driver, from AWS's own driver buckets (the instance role can read them)
if ($gpu -eq 'nvidia') {
  $bucket = 'ec2-windows-nvidia-drivers'
  foreach ($o in (Get-S3Object -BucketName $bucket -KeyPrefix latest -Region us-east-1)) {
    if ($o.Size -gt 0) { Copy-S3Object -BucketName $bucket -Key $o.Key -LocalFile ('C:\vmtest\nvidia\' + $o.Key) -Region us-east-1 | Out-Null }
  }
  $all = Get-ChildItem C:\vmtest\nvidia -Recurse -Filter *.exe
  $exe = ($all | Where-Object Name -like '*server2025*' | Select-Object -First 1)
  if (-not $exe) { $exe = ($all | Where-Object Name -like '*server2022*' | Select-Object -First 1) }
  if (-not $exe) { $exe = $all | Select-Object -First 1 }
  Write-Output "NVIDIA driver: $($exe.FullName)"
  Start-Process $exe.FullName -Wait -ArgumentList '-s -noreboot -clean'
  New-Item -Force 'HKLM:\SOFTWARE\NVIDIA Corporation\Global\GridLicensing' | Out-Null
  New-ItemProperty -Force -Path 'HKLM:\SOFTWARE\NVIDIA Corporation\Global\GridLicensing' -Name NvCplDisableManageLicensePage -Value 1 -PropertyType DWord | Out-Null
}
if ($gpu -eq 'amd') {
  $bucket = 'ec2-amd-windows-drivers'
  foreach ($o in (Get-S3Object -BucketName $bucket -KeyPrefix latest -Region us-east-1)) {
    if ($o.Size -gt 0) { Copy-S3Object -BucketName $bucket -Key $o.Key -LocalFile ('C:\vmtest\amd\' + $o.Key) -Region us-east-1 | Out-Null }
  }
  Get-ChildItem C:\vmtest\amd -Recurse -Filter *.zip | ForEach-Object { Expand-Archive $_.FullName -DestinationPath C:\vmtest\amd\driver -Force }
  pnputil /add-driver C:\vmtest\amd\driver\*.inf /install /subdirs
}

# 6. After the restart (driver loaded, desktop logged on), say so
$ready = New-ScheduledTaskAction -Execute powershell.exe -Argument '-NoProfile -Command "Start-Sleep 45; Set-Content C:\vmtest\ready.txt (Get-Date -Format o)"'
Register-ScheduledTask -TaskName only3d-ready -Action $ready -Trigger (New-ScheduledTaskTrigger -AtStartup) -User SYSTEM -RunLevel Highest -Force | Out-Null
Write-Output 'setup done, restarting'
Stop-Transcript
Restart-Computer -Force
</powershell>
