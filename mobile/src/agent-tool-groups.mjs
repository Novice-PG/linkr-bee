const groups = {
  web: { description: "Read public documentation and find official Radxa pages.", tools: ["read_web_page", "search_radxa_docs"] },
  verification: { description: "Compare target files or services with explicit expectations.", tools: ["verify_target_file", "verify_target_service"] },
  downloads: { description: "Probe transfer tools and download to the target or this host. Stop after download.", tools: ["probe_download_tools", "download_to_target", "download_to_computer"] },
  accessory: { description: "Read Bee diagnostics or change its UART, WiFi and WebDAV settings. Changes still need approval.", tools: ["get_accessory_diagnostics", "set_uart_config", "wifi_scan", "set_wifi", "set_webdav"] },
  notes: { description: "Remember a confirmed, durable target fact with supporting evidence.", tools: ["remember_target_note"] },
  files: { description: "Read bounded target file pages after probing dd and base64.", tools: ["read_target_file"] },
  watch: { description: "Observe boot loops, crashes and explicit log patterns without sending input.", tools: ["watch_serial_output"] },
  profile: { description: "Discover an unknown target's basic identity and capabilities; prefer focused probe_tools on known targets.", tools: ["probe_device_profile"] },
};

export const CORE_TOOL_NAMES = new Set(["load_tools", "load_skill", "get_task_checkpoint", "update_task_plan",
  "get_device_status", "read_serial_log", "search_serial_log", "probe_tools", "send_serial_input",
  "run_shell_command", "inspect_serial_execution", "monitor_serial_execution", "wait_for_serial_output"]);

export function toolGroupCatalog(availableNames) {
  const available = new Set(availableNames);
  return Object.entries(groups).flatMap(([name, group]) => {
    const tools = group.tools.filter(tool => available.has(tool));
    return tools.length ? [{ name, description: group.description, tools }] : [];
  });
}

export function groupForTool(name) {
  return Object.entries(groups).find(([, group]) => group.tools.includes(name))?.[0];
}

export function toolCatalogPrompt(catalog) {
  return "Optional tool groups (load_tools enables schemas, never permissions):\n" + catalog.map(group =>
    `- ${group.name}: ${group.description} (${group.tools.join(", ")})`).join("\n");
}
