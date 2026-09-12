# Linkr Bee documentation

These documents are for people building, integrating, or maintaining Linkr Bee.
If you just want to use the accessory, start with the project README instead:
[English](../README.md) · [中文](../README.zh-CN.md).

| Document | Audience | Contents |
| --- | --- | --- |
| [Project README](../README.md) · [中文](../README.zh-CN.md) | Users | What Linkr Bee is for, what it does, setup, everyday use, and troubleshooting |
| [Development guide](DEVELOPMENT.md) | Firmware and client developers | Build, flash, UART integration, test modes, terminal tools, validation status, and Kconfig |
| [开发指南](DEVELOPMENT.zh-CN.md) | 固件与客户端开发者 | 构建、刷写、UART 集成、测试模式、终端工具、验证状态与 Kconfig |
| [BLE accessory API v1](LINKR_BLE_API.zh-CN.md) | Linkr and third-party integrators | GATT services, framed requests, Reliable UART, events, and acceptance checks |
| [BLE pairing / 蓝牙配对](BLE_PAIRING.md) | Users and client developers | GPIO1 authorization, persistent bonds, client steps, and recovery |
| [Hardware requirements](HARDWARE.md) | Board and hardware engineers | SoC, pinout, electrical requirements, test points, and production notes |
| [WiFi/BLE coexistence findings](WIFI_DEBUG_FINDINGS.md) | Firmware maintainers | Reproduction evidence, Zephyr patches, and regression checks |
| [Agent validation](AGENT_VALIDATION.zh-CN.md) | Firmware and client developers | Task recovery, device profiles, and what still needs hardware acceptance |
| [Android/iOS client](../mobile/README.md) | Mobile developers | Capacitor build and native platform requirements |
| [HarmonyOS NEXT client](../harmonyos/README.md) | HarmonyOS developers | DevEco build, signing, emulator status, and hardware-test boundary |
| [HarmonyOS bridge protocol](../harmonyos/BRIDGE_PROTOCOL.md) | Client implementers | ArkWeb-to-ArkTS JSON bridge contract |
