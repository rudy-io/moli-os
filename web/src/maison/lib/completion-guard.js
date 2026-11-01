/** An async result may change presentation only while its user intent is current. */
export function completionGuard() {
  let revision = 0;
  return {
    change() { revision += 1; },
    capture() {
      const ownRevision = revision;
      return () => ownRevision === revision;
    },
  };
}
