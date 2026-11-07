// One cancellable human approval for the exact gateway action requested.
// Each component owns its lifetime; navigating away must not replay the action.
export function gatewayApproval({ home, label, waiting, failed, current, perform, reason = () => 'configuration', freshPin = () => false }) {
  let alive = true;
  let heldRetry = null;
  function cancel() {
    if (heldRetry && home.held?.custom === heldRetry) home.held = null;
    heldRetry = null;
    waiting(false);
  }
  return {
    ask(intent) {
      if (!alive) return;
      if (home.held) { failed('auth_busy'); return; }
      waiting(true);
      const retry = async (pin) => {
        if (!alive || heldRetry !== retry) return;
        heldRetry = null;
        waiting(false);
        if (current(intent)) await perform(intent,true,pin);
      };
      heldRetry = retry;
      home.held = {
        label:label(intent),reason:reason(intent),freshPin:freshPin(intent),custom:retry,
        onCancel:() => {
          if (!alive || heldRetry !== retry) return;
          heldRetry = null;
          waiting(false);
          failed('cancelled');
        },
      };
    },
    cancel,
    destroy() {
      alive = false;
      cancel();
    },
  };
}
