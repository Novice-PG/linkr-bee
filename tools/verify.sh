#!/usr/bin/env sh
# Build every supported firmware and check the public host client.
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
boards=${BOARD:-"esp32c3_supermini esp32c5_devkitc/esp32c5/hpcore"}

python3 -m unittest discover -s "$repo_dir/tests" -v

for board in $boards; do
  build_name=$(printf '%s' "$board" | tr '/_' '--')
  build_dir="$repo_dir/build-verify-$build_name"
  west build -p always -d "$build_dir" -b "$board" "$repo_dir"
  config="$build_dir/zephyr/.config"
  grep -qx 'CONFIG_LINKR_BLE_BRIDGE_WIFI=y' "$config"
  grep -qx 'CONFIG_LINKR_BLE_BRIDGE_WIFI_OPERATION_TIMEOUT_MS=30000' "$config"
  grep -qx 'CONFIG_LINKR_BLE_BRIDGE_WEBDAV=y' "$config"
  grep -qx 'CONFIG_NETWORKING=y' "$config"
  grep -qx 'CONFIG_NET_TCP=y' "$config"
  grep -qx '# CONFIG_NET_IPV6 is not set' "$config"
  grep -qx 'CONFIG_HTTP_SERVER=y' "$config"
  grep -qx 'CONFIG_HTTP_SERVER_VERSION_1=y' "$config"
  grep -qx '# CONFIG_HTTP_SERVER_VERSION_2 is not set' "$config"
  grep -qx 'CONFIG_HTTP_SERVER_WEBSOCKET=y' "$config"
  grep -qx 'CONFIG_ZVFS_EVENTFD_MAX=2' "$config"
  grep -qx 'CONFIG_LINKR_BLE_BRIDGE_WS_BRIDGE=y' "$config"
  grep -qx 'CONFIG_LINKR_BLE_BRIDGE_UART_RX_DROP_NO_CONN=y' "$config"
  grep -qx 'CONFIG_BT_SMP=y' "$config"
  if grep -qx 'CONFIG_SOC_SERIES_ESP32C3=y' "$config"; then
    grep -qx 'CONFIG_ESP32_BT_CTLR_LE_SECURITY_ENABLE=y' "$config"
  fi
  grep -qx 'CONFIG_BT_SMP_APP_PAIRING_ACCEPT=y' "$config"
  grep -qx 'CONFIG_BT_BONDING_REQUIRED=y' "$config"
  grep -qx 'CONFIG_BT_SETTINGS=y' "$config"
  grep -qx 'CONFIG_BT_SMP_SC_PAIR_ONLY=y' "$config"
  grep -qx 'CONFIG_BT_MAX_PAIRED=8' "$config"
  grep -qx '# CONFIG_BT_KEYS_OVERWRITE_OLDEST is not set' "$config"
  grep -qx '# CONFIG_BT_ZEPHYR_NUS_DEFAULT_INSTANCE is not set' "$config"
done

python3 -m compileall -q "$repo_dir/tools/linkr_ble_terminal.py"
sh -n "$repo_dir/tools/serve_web.sh"
sh -n "$repo_dir/tools/flash_firmware.sh"
