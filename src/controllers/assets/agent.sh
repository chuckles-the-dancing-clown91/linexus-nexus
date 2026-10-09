#!/bin/sh
# Linexus agent installer, served by Nexus at $NEXUS_PUBLIC_URL/install/agent.sh
# (kept in step with linexus-agent/rmm-agent/deploy/install.sh):
#
#   curl -fsSL $NEXUS/install/agent.sh | NEXUS_URL=$NEXUS ENROLLMENT_TOKEN=nxe_... sh
#
# Steps, all idempotent (re-running upgrades the binary and keeps the
# agent's identity):
#   1. download rmm-agent-linux-<amd64|arm64> from $AGENT_BINARY_URL
#      (default $NEXUS_URL/install) to /usr/local/bin/linexus-agent;
#   2. write /etc/linexus/agent.env (0600) with NEXUS_URL, ENROLLMENT_TOKEN and
#      AGENT_STATE_FILE=/var/lib/linexus/agent-state.json, keeping any other
#      lines an operator added;
#   3. install /etc/systemd/system/linexus-agent.service, enable and
#      (re)start it.
# Once enrolled, the agent ignores ENROLLMENT_TOKEN: its identity and
# per-agent credential live in the state file.
set -eu
# @NEXUS_DEFAULT_URL@

die() { echo "linexus-agent install: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (pipe to 'sudo sh')"
[ -n "${NEXUS_URL:-}" ] || die "NEXUS_URL is required"
NEXUS_URL=${NEXUS_URL%/}
STATE_FILE=/var/lib/linexus/agent-state.json
if [ -z "${ENROLLMENT_TOKEN:-}" ] && [ ! -s "$STATE_FILE" ] && [ -z "${AGENT_TOKEN:-}" ]; then
  die "ENROLLMENT_TOKEN is required for a first install"
fi
command -v systemctl >/dev/null 2>&1 || die "systemd is required"

case "$(uname -m)" in
  x86_64 | amd64) ARCH=amd64 ;;
  aarch64 | arm64) ARCH=arm64 ;;
  *) die "unsupported architecture $(uname -m)" ;;
esac

BASE=${AGENT_BINARY_URL:-$NEXUS_URL/install}
BIN=/usr/local/bin/linexus-agent
TMP=$(mktemp)
trap 'rm -f "$TMP" "$TMP.env"' EXIT

echo "downloading $BASE/rmm-agent-linux-$ARCH"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL --retry 5 --retry-delay 3 -o "$TMP" "$BASE/rmm-agent-linux-$ARCH"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMP" "$BASE/rmm-agent-linux-$ARCH"
else
  die "curl or wget is required"
fi
# An ELF binary, not an error page.
[ "$(head -c 4 "$TMP" | od -An -c | tr -d ' ')" = "177ELF" ] || die "download is not a Linux binary"

changed=0
if ! cmp -s "$TMP" "$BIN" 2>/dev/null; then
  install -m 0755 "$TMP" "$BIN.new" && mv -f "$BIN.new" "$BIN"
  changed=1
fi

install -d -m 0755 /etc/linexus
install -d -m 0700 /var/lib/linexus
ENV=/etc/linexus/agent.env
managed='NEXUS_URL|ENROLLMENT_TOKEN|AGENT_STATE_FILE'
if [ -n "${AGENT_TOKEN:-}" ]; then managed="$managed|AGENT_TOKEN"; fi
if [ -n "${AGENT_HOSTGROUP:-}" ]; then managed="$managed|AGENT_HOSTGROUP"; fi
{
  echo "# Written by the Linexus agent installer. Other lines are kept on re-install."
  echo "NEXUS_URL=$NEXUS_URL"
  if [ -n "${ENROLLMENT_TOKEN:-}" ]; then echo "ENROLLMENT_TOKEN=$ENROLLMENT_TOKEN"; fi
  if [ -n "${AGENT_TOKEN:-}" ]; then echo "AGENT_TOKEN=$AGENT_TOKEN"; fi
  if [ -n "${AGENT_HOSTGROUP:-}" ]; then echo "AGENT_HOSTGROUP=$AGENT_HOSTGROUP"; fi
  echo "AGENT_STATE_FILE=$STATE_FILE"
  if [ -f "$ENV" ]; then
    grep -v -E "^(# Written by the Linexus agent installer|($managed)=)" "$ENV" || true
  fi
} > "$TMP.env"
if ! cmp -s "$TMP.env" "$ENV" 2>/dev/null; then
  install -m 0600 "$TMP.env" "$ENV"
  changed=1
fi

UNIT=/etc/systemd/system/linexus-agent.service
cat > "$TMP" <<'UNIT'
[Unit]
Description=Linexus infrastructure agent
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/linexus/agent.env
Environment=AGENT_STATE_FILE=/var/lib/linexus/agent-state.json
ExecStart=/usr/local/bin/linexus-agent
StateDirectory=linexus
StateDirectoryMode=0700
WorkingDirectory=/var/lib/linexus
UMask=0022
Restart=always
RestartSec=10
TimeoutStopSec=30
KillMode=process

[Install]
WantedBy=multi-user.target
UNIT
if ! cmp -s "$TMP" "$UNIT" 2>/dev/null; then
  install -m 0644 "$TMP" "$UNIT"
  changed=1
fi

systemctl daemon-reload
systemctl enable linexus-agent.service >/dev/null 2>&1
if [ "$changed" -eq 1 ] || ! systemctl is-active --quiet linexus-agent.service; then
  systemctl restart linexus-agent.service
fi
echo "linexus-agent installed and running — journalctl -u linexus-agent -f"
