// A preference is captured at connection time; it never changes an active stream.
export function normalizeUartMode(value) {
  return value === "reliable" ? "reliable" : "interactive";
}

export function uartTransportPlan(mode, { reliable = false, preferNus = false } = {}) {
  if (!reliable || preferNus) return ["nus"];
  return normalizeUartMode(mode) === "reliable" ? ["reliable", "nus"] : ["nus", "reliable"];
}
