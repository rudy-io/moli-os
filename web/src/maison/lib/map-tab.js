// Whether the Carte tab has something to show: where people are comes from their
// phones (the Moli app), so a house with no phone and nobody located sees no empty map.
// It comes back on its own once a phone is paired.

/** True when a phone driver runs or someone's position is known. */
export function mapUseful(drivers, devices) {
  if ((drivers ?? []).some((d) => d?.kind === 'phones')) return true;
  return Object.values(devices ?? {}).some(
    (d) => typeof d?.id === 'string' && d.id.startsWith('personnes:') && Number.isFinite(d.state?.latitude?.value),
  );
}
