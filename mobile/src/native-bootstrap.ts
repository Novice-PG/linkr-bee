import { BleClient } from "@capacitor-community/bluetooth-le";
import { Capacitor } from "@capacitor/core";

import { installLinkrMcpBridge } from "./mcp-native";

type DeviceDescriptor = { id: string; name: string };
type ScannedDevice = { id: string; name: string; rssi: number | null };
type ScanDevicesOptions = { services?: string[]; durationMs?: number };
type RequestDeviceOptions = {
  filters?: Array<{ services?: string[]; namePrefix?: string }>;
  optionalServices?: string[];
};

/* The plugin's stopLEScan has to run even if the scan itself fails, and a
 * caller must never be able to hang the MCP request for minutes. Keep the
 * window short and bounded; 50 entries is far more BLE peripherals than a
 * nearby-board list can meaningfully show. */
const DEFAULT_SCAN_DURATION_MS = 6000;
const MIN_SCAN_DURATION_MS = 500;
const MAX_SCAN_DURATION_MS = 15000;
const MAX_SCAN_RESULTS = 50;

type NativeBleBackend = {
  platform: string;
  isAvailable(): boolean;
  initialize(): Promise<void>;
  requestDevice(options: RequestDeviceOptions): Promise<DeviceDescriptor>;
  getDevices(deviceIds: string[]): Promise<DeviceDescriptor[]>;
  scanDevices(options?: ScanDevicesOptions): Promise<ScannedDevice[]>;
  connect(
    deviceId: string,
    onDisconnect?: (deviceId: string) => void,
  ): Promise<void>;
  disconnect(deviceId: string): Promise<void>;
  isConnected(): boolean;
  read(
    deviceId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<DataView>;
  write(
    deviceId: string,
    serviceUuid: string,
    characteristicUuid: string,
    value: Uint8Array,
    withResponse?: boolean,
  ): Promise<void>;
  startNotifications(
    deviceId: string,
    serviceUuid: string,
    characteristicUuid: string,
    callback: (value: Uint8Array) => void,
  ): Promise<void>;
  stopNotifications(
    deviceId: string,
    serviceUuid: string,
    characteristicUuid: string,
  ): Promise<void>;
};

declare global {
  interface Window {
    LinkrNativeBle?: NativeBleBackend;
    LinkrNativeBleReady?: Promise<void>;
  }
}

function copyBytes(value: DataView): Uint8Array {
  return new Uint8Array(
    value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength),
  );
}

function asDataView(value: Uint8Array): DataView {
  return new DataView(
    value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength),
  );
}

function deviceName(device: { name?: string; deviceId: string }): string {
  return device.name || device.deviceId;
}

function scanDuration(value: number | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DEFAULT_SCAN_DURATION_MS;
  }
  return Math.min(
    MAX_SCAN_DURATION_MS,
    Math.max(MIN_SCAN_DURATION_MS, Math.round(value)),
  );
}

function scanResultName(result: {
  localName?: string;
  device?: { name?: string };
}): string {
  return result.localName || result.device?.name || "";
}

function scanResultRssi(result: { rssi?: number }): number | null {
  return typeof result.rssi === "number" && Number.isFinite(result.rssi)
    ? result.rssi
    : null;
}

function createNativeBleBackend(): NativeBleBackend {
  let initializePromise: Promise<void> | null = null;
  let connectedDeviceId: string | null = null;

  /* Only a successful initialization may stay cached. Caching a rejected
   * promise would poison every later BLE call: one transient failure (adapter
   * off, a permission race) would leave the app unable to connect until it is
   * killed. */
  const ensureInitialized = (): Promise<void> => {
    if (initializePromise) {
      return initializePromise;
    }
    let guarded: Promise<void>;
    const attempt: Promise<void> = BleClient.initialize({
      androidNeverForLocation: true,
    });
    guarded = attempt.catch((error: unknown) => {
      if (initializePromise === guarded) {
        initializePromise = null;
      }
      throw error;
    });
    initializePromise = guarded;
    return guarded;
  };

  return {
    platform: Capacitor.getPlatform(),

    isAvailable() {
      return true;
    },

    initialize: ensureInitialized,

    async requestDevice(options) {
      await ensureInitialized();
      const services = Array.from(
        new Set(options.filters?.flatMap((filter) => filter.services || []) || []),
      );
      const namePrefixes = Array.from(
        new Set(options.filters?.flatMap((filter) => filter.namePrefix || []) || []),
      );
      /* The plugin takes one filter shape, while Web Bluetooth takes an OR of
       * several. Prefer the service filter when the caller supplied one: it
       * finds every current board regardless of how the owner renamed it,
       * including Chinese and emoji names. The name prefixes remain a fallback
       * for callers that only have names (the oldest NUS-only firmware is
       * discoverable through the Web/TUI clients, which can scan both). */
      const filter = services.length
        ? { services }
        : { namePrefix: namePrefixes[0] || "Linkr BLE UART" };
      const device = await BleClient.requestDevice({
        ...filter,
        optionalServices: options.optionalServices || [],
      });
      return { id: device.deviceId, name: deviceName(device) };
    },

    async getDevices(deviceIds) {
      if (!deviceIds.length) {
        return [];
      }
      await ensureInitialized();
      const devices = await BleClient.getDevices(deviceIds);
      return devices.map((device) => ({
        id: device.deviceId,
        name: deviceName(device),
      }));
    },

    /* Headless discovery for the on-phone MCP server. Web Bluetooth can only
     * show a browser chooser, so only the native backend implements this.
     * The scan runs for a bounded window because requestLEScan streams
     * advertisements until stopLEScan is called. */
    async scanDevices(options = {}) {
      await ensureInitialized();
      const services = Array.from(
        new Set((options.services || []).filter((service) => Boolean(service))),
      );
      const scanOptions: { allowDuplicates: boolean; services?: string[] } = {
        allowDuplicates: false,
      };
      if (services.length) {
        scanOptions.services = services;
      }
      const found = new Map<string, ScannedDevice>();
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await BleClient.requestLEScan(scanOptions, (result) => {
          const deviceId = result.device?.deviceId;
          /* Results without a device id cannot be connected to, so they are
           * useless to the caller; drop them instead of returning blanks. */
          if (!deviceId) return;
          found.set(deviceId, {
            id: deviceId,
            name: scanResultName(result),
            rssi: scanResultRssi(result),
          });
        });
        await new Promise<void>((resolve) => {
          timer = setTimeout(resolve, scanDuration(options.durationMs));
        });
      } finally {
        if (timer !== undefined) {
          clearTimeout(timer);
        }
        /* A failing stop must not hide a scan error or discard results that
         * were already found, so this is best effort. */
        await BleClient.stopLEScan().catch(() => {});
      }
      return Array.from(found.values()).slice(0, MAX_SCAN_RESULTS);
    },

    async connect(deviceId, onDisconnect) {
      await ensureInitialized();
      await BleClient.connect(deviceId, (disconnectedId) => {
        if (connectedDeviceId === disconnectedId) {
          connectedDeviceId = null;
        }
        onDisconnect?.(disconnectedId);
      });
      connectedDeviceId = deviceId;
      try {
        // Android exposes explicit bonding; iOS negotiates it when the first
        // encrypted characteristic is read. Never remove an existing bond.
        if (Capacitor.getPlatform() === "android" && !(await BleClient.isBonded(deviceId))) {
          await BleClient.createBond(deviceId, { timeout: 60000 });
        }
        if (connectedDeviceId !== deviceId) throw new Error("Bluetooth disconnected during pairing");
      } catch (error) {
        await BleClient.disconnect(deviceId).catch(() => {});
        connectedDeviceId = null;
        throw error;
      }
    },

    async disconnect(deviceId) {
      await BleClient.disconnect(deviceId);
      if (connectedDeviceId === deviceId) {
        connectedDeviceId = null;
      }
    },

    isConnected() {
      return connectedDeviceId !== null;
    },

    async read(deviceId, serviceUuid, characteristicUuid) {
      await ensureInitialized();
      return BleClient.read(deviceId, serviceUuid, characteristicUuid, { timeout: 60000 });
    },

    async write(
      deviceId,
      serviceUuid,
      characteristicUuid,
      value,
      withResponse = true,
    ) {
      await ensureInitialized();
      const data = asDataView(value);
      if (withResponse) {
        await BleClient.write(deviceId, serviceUuid, characteristicUuid, data);
      } else {
        await BleClient.writeWithoutResponse(
          deviceId,
          serviceUuid,
          characteristicUuid,
          data,
        );
      }
    },

    async startNotifications(
      deviceId,
      serviceUuid,
      characteristicUuid,
      callback,
    ) {
      await ensureInitialized();
      await BleClient.startNotifications(
        deviceId,
        serviceUuid,
        characteristicUuid,
        (value) => callback(copyBytes(value)),
      );
    },

    async stopNotifications(deviceId, serviceUuid, characteristicUuid) {
      await BleClient.stopNotifications(
        deviceId,
        serviceUuid,
        characteristicUuid,
      );
    },
  };
}

// Install the native backend synchronously. app.js reads window.LinkrNativeBle
// while its module body runs (platform detection, support text), and that runs
// before any microtask. Deferring the assignment to a promise callback made
// those reads fall back to the web backend and misreport the platform.
if (Capacitor.isNativePlatform()) {
  window.LinkrNativeBle = createNativeBleBackend();
  // The MCP transport is Android-only; the installer picks the platform.
  installLinkrMcpBridge();
}
window.LinkrNativeBleReady = Promise.resolve();
