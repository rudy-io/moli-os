// A photo for each room, kept with the house's plan (the plan's images
// folder, its « photos » list): the Rooms page shows the room in it.
import { asHuman } from './auto.svelte.js';
import { t } from '../../lib/i18n.svelte.js';

export const photos = $state({ map: {}, loaded: false });

const HEADERS = { 'x-moli-origin': 'ui' };

async function fail(res) {
  const body = await res.json().catch(() => ({}));
  const err = new Error(body.error ?? res.statusText);
  err.status = res.status;
  throw err;
}

export async function loadPhotos() {
  try {
    const res = await fetch('/api/plan', { headers: HEADERS });
    if (res.ok) photos.map = (await res.json()).photos ?? {};
  } catch {
    /* no plan yet: no photos */
  }
  photos.loaded = true;
}

export const photoOf = (room) => (photos.map[room] ? `/api/plan/images/${photos.map[room]}` : null);

/** Sends the photo, then writes it in the plan (as a person: « c'est moi »). */
export function setPhoto(room, file) {
  return asHuman(t('maison.scene.label_photo', { name: room }), async () => {
    const up = await fetch('/api/plan/images', { method: 'POST', headers: { ...HEADERS, 'content-type': file.type }, body: file });
    if (!up.ok) await fail(up);
    const { image } = await up.json();
    const cur = await fetch('/api/plan', { headers: HEADERS });
    if (!cur.ok) await fail(cur);
    const plan = await cur.json();
    plan.photos = { ...(plan.photos ?? {}), [room]: image };
    const put = await fetch('/api/plan', { method: 'PUT', headers: { ...HEADERS, 'content-type': 'application/json' }, body: JSON.stringify(plan) });
    if (!put.ok) await fail(put);
    photos.map = plan.photos;
  });
}
