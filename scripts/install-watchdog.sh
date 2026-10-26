#!/bin/bash
# Installs (or with --uninstall removes) a launchd agent that runs the VM and
# restarts it when it crashes or hangs (see vmm-run.sh). With it installed,
# vmm-up.sh starts the VM through launchd, and a VM that was running when you
# logged out starts again at login. `vmm-up.sh --stop` / `orbit down` stop it
# for good. ORBIT_INSTANCE selects the instance. `make install` runs this.
# Re-running it leaves a running VM alone: launchd reads the plist when the
# agent is loaded, so a changed one takes effect at the next login, or now
# with --reload (which shuts the VM down and lets vmm-up.sh start it again).
set -eu
LABEL="com.orbit.vmm${ORBIT_INSTANCE:+.$ORBIT_INSTANCE}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
D="$HOME/.orbit/vmm${ORBIT_INSTANCE:+-$ORBIT_INSTANCE}"
RUN="$HOME/.orbit/bin/vmm-run.sh"   # the installed copy, never the checkout

if [ "${1:-}" = --uninstall ]; then
  rm -f "$D/want-running"   # removing the agent means: stay down
  # Unloading sends SIGTERM, which vmm-run.sh turns into a clean shutdown.
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  echo "removed $LABEL (the VM, if it was running, was shut down)"
  exit 0
fi

[ -x "$RUN" ] || { echo "run make install first" >&2; exit 1; }
mkdir -p "$HOME/Library/LaunchAgents" "$D"
NEW="$PLIST.new"
cat > "$NEW" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$RUN</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>ORBIT_WATCHDOG</key><string>1</string>
    <key>ORBIT_INSTANCE</key><string>${ORBIT_INSTANCE:-}</string>
  </dict>
  <!-- Restart only after an unexpected exit; vmm-run.sh exits 0 when stopped on purpose. -->
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>ExitTimeOut</key><integer>90</integer>
  <key>StandardOutPath</key><string>$D/watchdog.log</string>
  <key>StandardErrorPath</key><string>$D/watchdog.log</string>
</dict>
</plist>
PLIST
if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 && [ "${1:-}" != --reload ]; then
  # Loaded already. Unloading would shut the VM down, so only swap the file.
  if cmp -s "$NEW" "$PLIST"; then rm -f "$NEW"; echo "$LABEL is already installed"; exit 0; fi
  mv -f "$NEW" "$PLIST"
  echo "updated $PLIST; it takes effect at the next login, or now with $0 --reload (restarts the VM)"
  exit 0
fi
mv -f "$NEW" "$PLIST"
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "installed $LABEL; start the VM with orbit up (log: $D/watchdog.log, crashes: $D/crashes)"
