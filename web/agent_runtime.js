// The native/Vite builds replace this with the lazily loaded Pi runtime.
// Plain static web hosting remains usable without npm dependencies.
export const available = false;
export async function createSerialAgent() {
  throw new Error("Agent runtime unavailable");
}
