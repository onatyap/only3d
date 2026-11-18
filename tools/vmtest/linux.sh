#!/bin/bash
# only3d test VM setup: Ubuntu 24.04 with the GNOME desktop on X11. vm.sh fills in the
# __PLACEHOLDERS__ and passes this as EC2 user data. Log: /var/log/only3d-vmtest.log
exec > /var/log/only3d-vmtest.log 2>&1
set -x
DEADLINE=__DEADLINE__
GPU=__GPU__
mkdir -p /var/lib/only3d-vmtest
# installing the desktop swaps the network stack and DNS drops out for a while: wait, retry
dns() { for _ in $(seq 60); do getent hosts "$1" >/dev/null && return 0; systemctl restart systemd-resolved 2>/dev/null; sleep 5; done; }
retry() { for _ in 1 2 3 4 5; do "$@" && return 0; sleep 10; done; return 1; }

# 1. Power off at the deadline (EC2 then deletes the VM), checked every 2 minutes and at boot
echo "$DEADLINE" > /var/lib/only3d-vmtest/deadline
cat > /usr/local/sbin/only3d-deadline <<'EOF'
#!/bin/sh
[ "$(date +%s)" -ge "$(cat /var/lib/only3d-vmtest/deadline)" ] && /sbin/poweroff
exit 0
EOF
chmod +x /usr/local/sbin/only3d-deadline
printf '%s\n' '*/2 * * * * root /usr/local/sbin/only3d-deadline' '@reboot root sleep 60; /usr/local/sbin/only3d-deadline' > /etc/cron.d/only3d-deadline

# 2. The NVIDIA driver first: once the desktop is in, Ubuntu's own nouveau driver claims the GPU
# and the NVIDIA installer refuses to run
export DEBIAN_FRONTEND=noninteractive
if [ "$GPU" = nvidia ]; then
  printf 'blacklist vga16fb\nblacklist nouveau\nblacklist rivafb\nblacklist nvidiafb\nblacklist rivatv\noptions nouveau modeset=0\n' > /etc/modprobe.d/blacklist-nouveau.conf
  retry apt-get update
  retry apt-get install -y gcc make "linux-headers-$(uname -r)"
  retry snap install aws-cli --classic
  mkdir -p /tmp/nvidia && cd /tmp/nvidia
  retry /snap/bin/aws s3 cp --recursive --only-show-errors --region us-east-1 s3://ec2-linux-nvidia-drivers/latest/ .
  sh ./NVIDIA-Linux-x86_64*.run --silent --no-questions
  cp /etc/nvidia/gridd.conf.template /etc/nvidia/gridd.conf
  printf 'IgnoreSP=FALSE\nEnableUI=FALSE\n' >> /etc/nvidia/gridd.conf
  cd /
fi

# 3. A desktop like a user's: GNOME on X11 (DCV console sessions need X), logged on by itself.
# No libfuse2: a stock 24.04 doesn't have it, and the AppImage should run anyway.
# the desktop brings NetworkManager, which would take the network card away from the cloud's
# own network setup (no SSH, no DNS) until a restart: keep it off the hardware
mkdir -p /etc/NetworkManager/conf.d
printf '[keyfile]\nunmanaged-devices=*\n' > /etc/NetworkManager/conf.d/10-only3d-vmtest.conf
retry apt-get update
retry apt-get install -y ubuntu-desktop-minimal gdm3 mesa-vulkan-drivers mesa-utils vulkan-tools python3
cat > /etc/gdm3/custom.conf <<'EOF'
[daemon]
WaylandEnable=false
AutomaticLoginEnable=true
AutomaticLogin=ubuntu
EOF
mkdir -p /etc/dconf/profile /etc/dconf/db/local.d
printf 'user-db:user\nsystem-db:local\n' > /etc/dconf/profile/user
cat > /etc/dconf/db/local.d/00-only3d-vmtest <<'EOF'
[org/gnome/desktop/screensaver]
lock-enabled=false
[org/gnome/desktop/session]
idle-delay=uint32 0
[org/gnome/desktop/lockdown]
disable-lock-screen=true
EOF
dconf update
systemctl set-default graphical.target
[ "$GPU" = nvidia ] && update-initramfs -u
dns archive.ubuntu.com
dns d1uj6qtbmh3dt5.cloudfront.net
retry apt-get update

# 4. The display: the NVIDIA GPU itself, or a dummy screen drawn in software
if [ "$GPU" = nvidia ]; then
  rm -f /etc/X11/XF86Config*
  nvidia-xconfig --preserve-busid --enable-all-gpus --connected-monitor=DFP-0
else
  retry apt-get install -y xserver-xorg-video-dummy
  cat > /etc/X11/xorg.conf <<'EOF'
Section "Device"
    Identifier "DummyDevice"
    Driver "dummy"
    VideoRam 256000
EndSection
Section "Monitor"
    Identifier "DummyMonitor"
    HorizSync 5.0 - 1000.0
    VertRefresh 5.0 - 200.0
    Modeline "1920x1080" 23.53 1920 1952 2040 2072 1080 1106 1108 1135
EndSection
Section "Screen"
    Identifier "DummyScreen"
    Device "DummyDevice"
    Monitor "DummyMonitor"
    DefaultDepth 24
    SubSection "Display"
        Depth 24
        Modes "1920x1080"
        Virtual 1920 1080
    EndSubSection
EndSection
EOF
fi

# 5. Amazon DCV (free on EC2), sharing the console, with no login of its own: it's only
# reachable through the SSH tunnel
cd /tmp
retry wget -q https://d1uj6qtbmh3dt5.cloudfront.net/nice-dcv-ubuntu2404-x86_64.tgz
tar xzf nice-dcv-ubuntu2404-x86_64.tgz
cd nice-dcv-*-ubuntu2404-x86_64
retry apt-get install -y ./nice-dcv-server_*.deb ./nice-dcv-web-viewer_*.deb ./nice-xdcv_*.deb
usermod -aG video dcv
python3 - <<'EOF'
import configparser
p = "/etc/dcv/dcv.conf"
c = configparser.ConfigParser(interpolation=None, strict=False)
c.optionxform = str
c.read(p)
for sec, key, val in [("security", "authentication", '"none"'),
                      ("session-management", "create-session", "true"),
                      ("session-management/automatic-console-session", "owner", '"ubuntu"')]:
    if not c.has_section(sec):
        c.add_section(sec)
    c.set(sec, key, val)
with open(p, "w") as f:
    c.write(f)
EOF
printf '\n%%any%% allow builtin\n' >> /etc/dcv/default.perm
# the tester's own certificate, the same on every VM, so a browser accepts it once
echo '__DCV_CERT__' | base64 -d > /etc/dcv/dcv.pem
echo '__DCV_KEY__' | base64 -d > /etc/dcv/dcv.key
chown dcv:dcv /etc/dcv/dcv.pem /etc/dcv/dcv.key
chmod 600 /etc/dcv/dcv.key
systemctl enable dcvserver

# 6. After the restart (driver loaded, desktop logged on), say so
cat > /etc/systemd/system/only3d-ready.service <<'EOF'
[Unit]
Description=only3d test VM is ready
After=graphical.target dcvserver.service
[Service]
Type=oneshot
ExecStartPre=/bin/sleep 30
ExecStart=/bin/sh -c 'date -Is > /var/lib/only3d-vmtest/ready'
[Install]
WantedBy=graphical.target
EOF
systemctl enable only3d-ready
echo "setup done, restarting"
reboot
