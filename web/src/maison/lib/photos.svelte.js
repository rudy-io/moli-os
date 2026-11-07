// A photo for each room (and each page's banner, under « @<page> »: no
// room is named so), kept by the house apart from the plan: the household
// sees them, a guest does not.
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

let loading = null;
export function loadPhotos() {
  loading ??= (async () => {
    try {
      const res = await fetch('/api/photos', { headers: HEADERS });
      if (res.ok) photos.map = (await res.json()) ?? {};
    } catch {
      /* a guest, or no house folder: no photos */
    }
    photos.loaded = true;
  })();
  return loading;
}

export const photoOf = (room) => (photos.map[room] ? `/api/photos/images/${photos.map[room]}` : null);

/** The key a page's banner photo is kept under. */
export const pageKey = (page) => `@${page || 'accueil'}`;

/** A photo made small (1600 px, JPEG) before it is sent. */
export async function shrinkPhoto(file) {
  let img;
  try {
    img = await createImageBitmap(file);
  } catch {
    img = await new Promise((ok, ko) => {
      const el = new Image();
      const url = URL.createObjectURL(file);
      el.onload = () => (URL.revokeObjectURL(url), ok(el));
      el.onerror = (err) => (URL.revokeObjectURL(url), ko(err));
      el.src = url;
    });
  }
  const w0 = img.width || img.naturalWidth;
  const h0 = img.height || img.naturalHeight;
  const k = Math.min(1, 1600 / Math.max(w0, h0));
  const canvas = document.createElement('canvas');
  canvas.width = Math.round(w0 * k);
  canvas.height = Math.round(h0 * k);
  canvas.getContext('2d').drawImage(img, 0, 0, canvas.width, canvas.height);
  const blob = await new Promise((ok) => canvas.toBlob(ok, 'image/jpeg', 0.82));
  if (!blob) throw new Error('encode');
  return new File([blob], 'photo.jpg', { type: 'image/jpeg' });
}

/** Sends the photo (as a person: « c'est moi »).
 *  `name` is what the house's log says it is for. */
export function setPhoto(room, file, name = null) {
  const label = room.startsWith('@')
    ? t('maison.scene.label_page', { name: name ?? room.slice(1) })
    : t('maison.scene.label_photo', { name: room });
  return asHuman(label, async () => {
    // One exchange: the house swaps the photo and deletes the old one.
    const res = await fetch(`/api/photos/${encodeURIComponent(room)}`, {
      method: 'PUT',
      headers: { ...HEADERS, 'content-type': file.type },
      body: file,
    });
    if (!res.ok) await fail(res);
    photos.map = await res.json();
  });
}
