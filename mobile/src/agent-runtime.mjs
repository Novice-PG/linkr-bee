export const available = true;
export async function createSerialAgent(options) {
  const runtime = await import("./pi-agent.mjs");
  return runtime.createSerialAgent(options);
}
