#!/bin/bash
# Installs (or with --uninstall removes) the orbit-vmm health check as a
# launchd agent. launchd wakes it every 15 min; healthcheck.py itself decides
# whether a check is due (1 h, growing to 2 h and 4 h while healthy).
# Results: ~/.orbit/vmm/health.jsonl, last run's summary: ~/.orbit/vmm/health.log
set -eu
LABEL=com.orbit.healthcheck
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
# Runs the installed copy (make install), never the checkout: the checkout is
# usually in a shared directory that containers can write to.
HC="$HOME/.orbit/bin/healthcheck.py"
PY="$(command -v python3)"

if [ "${1:-}" = --uninstall ]; then
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  echo "removed $LABEL"
  exit 0
fi

[ -f "$HC" ] || { echo "run make install first" >&2; exit 1; }
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.orbit/vmm"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$PY</string><string>-I</string><string>$HC</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Applications/Docker.app/Contents/Resources/bin</string></dict>
  <key>StartInterval</key><integer>900</integer>
  <key>RunAtLoad</key><true/>
  <key>LowPriorityIO</key><true/>
  <key>StandardOutPath</key><string>$HOME/.orbit/vmm/health.log</string>
  <key>StandardErrorPath</key><string>$HOME/.orbit/vmm/health.log</string>
</dict>
</plist>
EOF
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "installed $LABEL ($PLIST); results in ~/.orbit/vmm/health.jsonl"
