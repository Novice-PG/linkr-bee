// The bundled xterm's public onData event combines input and protocol replies.
// Its core emits onUserInput immediately before onData for keyboard, IME and
// paste input. Isolate this private API dependency here; browser regression
// tests exercise the vendored xterm so an upgrade cannot silently change it.
export function subscribeTerminalInput(term, onData) {
  let userInput = false;
  const inputSubscription = term._core.coreService.onUserInput(() => {
    userInput = true;
  });
  const dataSubscription = term.onData((data) => {
    const fromUser = userInput;
    userInput = false;
    onData(data, { userInput: fromUser });
  });
  return {
    dispose() {
      inputSubscription.dispose();
      dataSubscription.dispose();
    },
  };
}
