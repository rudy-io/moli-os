// One cancellable human approval for the exact configuration action requested.
// Each component owns its lifetime; navigating away must not replay the action.
export function gatewayApproval({ home, label, waiting, failed, current, perform }) {
  let alive = true;
  let heldRetry = null;
  return {
    ask(intent) {
      if (!alive) return;
      if (home.held) { failed('auth_busy'); return; }
      waiting(true);
      const retry = async () => {
        if (!alive || heldRetry !== retry) return;
        heldRetry = null;
        waiting(false);
        if (current(intent)) await perform(intent,true);
      };
      heldRetry = retry;
      home.held = {
        label:label(),reason:'configuration',custom:retry,
        onCancel:() => {
          if (!alive || heldRetry !== retry) return;
          heldRetry = null;
          waiting(false);
          failed('cancelled');
        },
      };
    },
    destroy() {
      alive = false;
      if (heldRetry && home.held?.custom === heldRetry) home.held = null;
      heldRetry = null;
    },
  };
}
