#!/usr/bin/env bash
set -e

# IOT-Lights Server & Dashboard Startup Script
PORT="${1:-${PORT:-8080}}"

echo "=========================================================="
echo "          IOT-Lights | Audio Beat-Sync Lighting           "
echo "=========================================================="

# 1. Dependency checks
if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: Rust and Cargo are required but not found in PATH."
    echo "Install Rust via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

echo "[*] Checking Rust toolchain..."
rustc --version
cargo --version

# 2. Determine LAN IP address for mobile access
LAN_IP=""
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS primary interfaces
    LAN_IP=$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)
    if [ -z "$LAN_IP" ]; then
        LAN_IP=$(ifconfig 2>/dev/null | grep "inet " | grep -v 127.0.0.1 | awk '{print $2}' | head -n 1 || true)
    fi
else
    # Linux
    LAN_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || true)
    if [ -z "$LAN_IP" ]; then
        LAN_IP=$(ip route get 1.1.1.1 2>/dev/null | awk '{print $7}' || true)
    fi
fi

if [ -z "$LAN_IP" ]; then
    LAN_IP="127.0.0.1"
fi

echo "=========================================================="
echo " Server Port  : ${PORT}"
echo " Local URL    : http://localhost:${PORT}"
echo " Mobile / LAN : http://${LAN_IP}:${PORT}"
echo " WebSocket    : ws://${LAN_IP}:${PORT}/ws"
echo "=========================================================="
echo "[*] Building and starting IOT-Lights service..."
echo ""

exec cargo run --bin iot-lights -- serve --port "${PORT}"
