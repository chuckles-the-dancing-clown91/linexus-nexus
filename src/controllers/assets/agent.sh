#!/bin/sh
# Linexus agent installer, served by Nexus at $NEXUS_PUBLIC_URL/install/agent.sh
# (kept in step with linexus-agent/rmm-agent/deploy/install.sh):
#
#   curl -fsSL $NEXUS/install/agent.sh | NEXUS_URL=$NEXUS ENROLLMENT_TOKEN=nxe_... sh
#
# Optional:
#   NEXUS_CA_PEM=<PEM text> or NEXUS_CA_FILE=<path>  the CA that signed Nexus's
#       TLS certificate; saved to /etc/linexus/nexus-ca.pem and handed to the
#       agent as LINEXUS_CA_FILE.
#   NEXUS_SIGNING_PUBKEY=<base64>  the Ed25519 key the agent verifies plans
#       with (LINEXUS_SIGNING_PUBKEY). Defaults to the key of the Nexus that
#       served this script, embedded when it was served.
#   The agent's own names (LINEXUS_CA_PEM, LINEXUS_CA_FILE,
#   LINEXUS_SIGNING_PUBKEY, as rmm-agent/deploy/install.sh takes them) are
#   accepted too; the NEXUS_* names win. Empty values count as unset.
#
# Steps, all idempotent (re-running upgrades the binary and keeps the
# agent's identity):
#   1. download rmm-agent-linux-<amd64|arm64> from $AGENT_BINARY_URL
#      (default $NEXUS_URL/install) to /usr/local/bin/linexus-agent;
#   2. write /etc/linexus/agent.env (0600) with NEXUS_URL, ENROLLMENT_TOKEN,
#      AGENT_STATE_FILE=/var/lib/linexus/agent-state.json,
#      LINEXUS_SIGNING_PUBKEY and (with a CA) LINEXUS_CA_FILE, keeping any
#      other lines an operator added;
#   3. install /etc/systemd/system/linexus-agent.service, enable and
#      (re)start it.
# Once enrolled, the agent ignores ENROLLMENT_TOKEN: its identity and
# per-agent credential live in the state file.
set -eu
# @NEXUS_DEFAULT_URL@
# @NEXUS_DEFAULT_SIGNING_PUBKEY@

die() { echo "linexus-agent install: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (pipe to 'sudo sh')"
[ -n "${NEXUS_URL:-}" ] || die "NEXUS_URL is required"
NEXUS_URL=${NEXUS_URL%/}
STATE_FILE=/var/lib/linexus/agent-state.json
if [ -z "${ENROLLMENT_TOKEN:-}" ] && [ ! -s "$STATE_FILE" ] && [ -z "${AGENT_TOKEN:-}" ]; then
  die "ENROLLMENT_TOKEN is required for a first install"
fi
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
NEXUS_CA_PEM=${NEXUS_CA_PEM:-${LINEXUS_CA_PEM:-}}
NEXUS_CA_FILE=${NEXUS_CA_FILE:-${LINEXUS_CA_FILE:-}}
SIGNING_PUBKEY=${NEXUS_SIGNING_PUBKEY:-${LINEXUS_SIGNING_PUBKEY:-}}
case "$SIGNING_PUBKEY" in
  *[!A-Za-z0-9+/=]*) die "NEXUS_SIGNING_PUBKEY is not base64" ;;
esac
if [ -n "${NEXUS_CA_PEM:-}" ] && [ -n "${NEXUS_CA_FILE:-}" ]; then
  die "set NEXUS_CA_PEM or NEXUS_CA_FILE, not both"
fi
if [ -n "${NEXUS_CA_FILE:-}" ] && [ ! -r "$NEXUS_CA_FILE" ]; then
  die "NEXUS_CA_FILE $NEXUS_CA_FILE is not readable"
fi

case "$(uname -m)" in
  x86_64 | amd64) ARCH=amd64 ;;
  aarch64 | arm64) ARCH=arm64 ;;
  *) die "unsupported architecture $(uname -m)" ;;
esac

BASE=${AGENT_BINARY_URL:-$NEXUS_URL/install}
BIN=/usr/local/bin/linexus-agent
TMP=$(mktemp)
trap 'rm -f "$TMP" "$TMP.env" "$TMP.ca"' EXIT

# The CA that signed Nexus's certificate, when one was given: checked now
# and used for the download, installed below.
if [ -n "${NEXUS_CA_PEM:-}" ]; then
  printf '%s\n' "$NEXUS_CA_PEM" > "$TMP.ca"
elif [ -n "${NEXUS_CA_FILE:-}" ]; then
  cat "$NEXUS_CA_FILE" > "$TMP.ca"
fi
if [ -s "$TMP.ca" ]; then
  grep -q -e "-----BEGIN CERTIFICATE-----" "$TMP.ca" || die "the CA is not a PEM certificate"
fi

echo "downloading $BASE/rmm-agent-linux-$ARCH"
if command -v curl >/dev/null 2>&1; then
  if [ -s "$TMP.ca" ]; then set -- --cacert "$TMP.ca"; else set --; fi
  curl -fsSL --retry 5 --retry-delay 3 "$@" -o "$TMP" "$BASE/rmm-agent-linux-$ARCH"
elif command -v wget >/dev/null 2>&1; then
  if [ -s "$TMP.ca" ]; then set -- --ca-certificate="$TMP.ca"; else set --; fi
  wget -q "$@" -O "$TMP" "$BASE/rmm-agent-linux-$ARCH"
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

# Keep the CA where the agent reads it (LINEXUS_CA_FILE).
CA_FILE=
if [ -s "$TMP.ca" ]; then
  CA_FILE=/etc/linexus/nexus-ca.pem
  if ! cmp -s "$TMP.ca" "$CA_FILE" 2>/dev/null; then
    install -m 0644 "$TMP.ca" "$CA_FILE"
    changed=1
  fi
fi

ENV=/etc/linexus/agent.env
managed='NEXUS_URL|ENROLLMENT_TOKEN|AGENT_STATE_FILE'
if [ -n "${AGENT_TOKEN:-}" ]; then managed="$managed|AGENT_TOKEN"; fi
if [ -n "${AGENT_HOSTGROUP:-}" ]; then managed="$managed|AGENT_HOSTGROUP"; fi
if [ -n "$CA_FILE" ]; then managed="$managed|LINEXUS_CA_FILE"; fi
if [ -n "$SIGNING_PUBKEY" ]; then managed="$managed|LINEXUS_SIGNING_PUBKEY"; fi
{
  echo "# Written by the Linexus agent installer. Other lines are kept on re-install."
  echo "NEXUS_URL=$NEXUS_URL"
  if [ -n "${ENROLLMENT_TOKEN:-}" ]; then echo "ENROLLMENT_TOKEN=$ENROLLMENT_TOKEN"; fi
  if [ -n "${AGENT_TOKEN:-}" ]; then echo "AGENT_TOKEN=$AGENT_TOKEN"; fi
  if [ -n "${AGENT_HOSTGROUP:-}" ]; then echo "AGENT_HOSTGROUP=$AGENT_HOSTGROUP"; fi
  echo "AGENT_STATE_FILE=$STATE_FILE"
  if [ -n "$CA_FILE" ]; then echo "LINEXUS_CA_FILE=$CA_FILE"; fi
  if [ -n "$SIGNING_PUBKEY" ]; then echo "LINEXUS_SIGNING_PUBKEY=$SIGNING_PUBKEY"; fi
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
