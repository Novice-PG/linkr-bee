import assert from "node:assert/strict";
import test from "node:test";
import {
  accessoryChangeSummary,
  isDiagnosticsDone,
  parseInfoGroups,
  parseUartSettings,
  parseWebdavStatus,
  parseWifiStatus,
  redactCommand,
  replyStatus,
  setUartCommand,
  webdavCommand,
  wifiCommand,
} from "../../web/accessory_control.js";

test("UART commands are built from validated settings only", () => {
  assert.equal(setUartCommand({ baud: 115200 }), "@u=115200,8,n,1,none");
  assert.equal(setUartCommand({ baud: 9600, dataBits: 7, parity: "E", stopBits: 2, flow: "rtscts" }),
    "@u=9600,7,e,2,rtscts");
  for (const bad of [
    {}, { baud: 299 }, { baud: 3000001 }, { baud: 9600.5 }, { baud: "fast" },
    { baud: 9600, dataBits: 9 }, { baud: 9600, parity: "m" }, { baud: 9600, stopBits: 3 }, { baud: 9600, flow: "xon" },
  ]) {
    assert.throws(() => setUartCommand(bad), Error, JSON.stringify(bad));
  }
});

test("WiFi commands refuse anything the firmware parser would misread", () => {
  assert.equal(wifiCommand({ action: "connect", ssid: "Bench", password: "secret" }), "@w=Bench,secret");
  assert.equal(wifiCommand({ ssid: "Open" }), "@w=Open,");
  assert.equal(wifiCommand({ action: "off" }), "@w off");
  // A comma would move the rest of the SSID into the password field.
  assert.throws(() => wifiCommand({ ssid: "Bench,5G", password: "secret" }), /comma/);
  assert.throws(() => wifiCommand({ ssid: "x".repeat(33) }), /at most 32/);
  assert.throws(() => wifiCommand({ ssid: "Bench", password: "x".repeat(65) }), /at most 64/);
  assert.throws(() => wifiCommand({ ssid: "   " }), /required/);
  assert.throws(() => wifiCommand({ action: "reset", ssid: "Bench" }), /connect or off/);
});

test("WebDAV commands accept only plain HTTP endpoints", () => {
  assert.equal(webdavCommand({ url: "http://host/dav/" }), "@d=http://host/dav/");
  assert.equal(webdavCommand({ url: "https://host/dav/" }), "@d=https://host/dav/");
  assert.equal(webdavCommand({ action: "off" }), "@d off");
  assert.throws(() => webdavCommand({}), /required/);
  assert.throws(() => webdavCommand({ url: "ftp://host/dav/" }), /http/);
  assert.throws(() => webdavCommand({ url: `http://host/${"x".repeat(250)}` }), /at most 256/);
  assert.throws(() => webdavCommand({ url: "http://host/a b" }), /whitespace/);
});

test("credentials never survive into a display or a tool result", () => {
  assert.equal(redactCommand("@w=Bench,secret"), "@w=Bench,<redacted>");
  assert.equal(redactCommand("@w=Open,"), "@w=Open,<redacted>");
  assert.equal(redactCommand("@w off"), "@w off");
  assert.equal(redactCommand("@d=http://user:pass@host/dav/"), "@d=http://<redacted>@host/dav/");
  assert.equal(redactCommand("@d=http://host/dav/"), "@d=http://host/dav/");
  assert.equal(redactCommand("@u=9600,8,n,1,none"), "@u=9600,8,n,1,none");

  const summary = accessoryChangeSummary("@w=Bench,secret", "en");
  assert.match(summary, /Bench/);
  assert.doesNotMatch(summary, /secret/);
  assert.match(accessoryChangeSummary("@w=Bench,secret", "zh-CN"), /Bench/);
  assert.doesNotMatch(accessoryChangeSummary("@w=Bench,secret", "zh-CN"), /secret/);
  assert.equal(accessoryChangeSummary("@u=9600,8,n,1,none", "en"), "Set the bridge UART to 9600,8,n,1,none");
  assert.equal(accessoryChangeSummary("@d off", "zh-CN"), "关闭 WebDAV 日志上传");
});

test("firmware replies are read as OK or as an error the model can act on", () => {
  assert.deepEqual(replyStatus("OK uart=115200,8,N,1,none"), { ok: true, error: "" });
  assert.deepEqual(replyStatus("ERR format: @u=115200,8,n,1,n\r\n"), { ok: false, error: "ERR format: @u=115200,8,n,1,n" });
  assert.equal(replyStatus("").ok, false);
  assert.equal(replyStatus(undefined).ok, false);
});

test("status replies are parsed into the fields a change is verified against", () => {
  assert.deepEqual(parseUartSettings("OK uart=115200,8,N,1,none"), { baud: 115200, dataBits: 8, parity: "n", stopBits: 1, flow: "none" });
  assert.deepEqual(parseWifiStatus("OK wifi=connected,ssid=MyNet,ip=192.168.1.5"), { state: "connected", ssid: "MyNet", ip: "192.168.1.5" });
  assert.deepEqual(parseWifiStatus("OK wifi off"), { state: "off", ssid: "", ip: "" });
  assert.deepEqual(parseWebdavStatus("OK webdav=on,url=http://host/dav/"), { state: "on", url: "http://host/dav/" });
  assert.equal(parseUartSettings("OK"), null);
  assert.equal(parseWifiStatus("garbage"), null);
  assert.equal(parseWebdavStatus(""), null);
});

test("the @i? stream is grouped until the device reports completion", () => {
  const lines = [
    "@info fw version=0.2.0 zephyr=4.4.1",
    "@info sys uptime_ms=123456 owner=0 security=2",
    "@info uart dropped=0 dropped_no_conn=0 buffer=12/4096",
  ];
  assert.deepEqual(parseInfoGroups(lines), {
    fw: { version: "0.2.0", zephyr: "4.4.1" },
    sys: { uptime_ms: "123456", owner: "0", security: "2" },
    uart: { dropped: "0", dropped_no_conn: "0", buffer: "12/4096" },
  });
  assert.equal(isDiagnosticsDone("@info done"), true);
  assert.equal(isDiagnosticsDone("@info fw version=0.2.0"), false);
  assert.equal(parseInfoGroups(["@info done"]).done, undefined);
});
