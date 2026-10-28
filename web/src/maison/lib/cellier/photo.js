// Preparing a receipt photo before sending it: shrunk to what the reader
// needs, as a JPEG. `fitTicket` is pure (tested); `shrink` and `toBase64` use
// browser APIs.

const SHORT_SIDE = 1200;
const LONG_SIDE = 4000;

/** Size to shrink to so the short side is at most 1200 and the long side at
 *  most 4000, proportions kept, never enlarged. */
export function fitTicket(width, height) {
  const short = Math.min(width, height);
  const long = Math.max(width, height);
  const scale = Math.min(1, SHORT_SIDE / short, LONG_SIDE / long);
  if (scale === 1) return { width, height };
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

async function decode(file) {
  if (typeof createImageBitmap === 'function') {
    try {
      // `from-image` applies the EXIF rotation of a phone photo.
      return await createImageBitmap(file, { imageOrientation: 'from-image' });
    } catch {
      // Fall through to the <img> path.
    }
  }
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    return img;
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** The photo as a JPEG blob (quality 0.85) fitted by `fitTicket`.
 *  @param {Blob} file
 *  @returns {Promise<Blob>} */
export async function shrink(file) {
  const image = await decode(file);
  const source = { width: image.naturalWidth ?? image.width, height: image.naturalHeight ?? image.height };
  const { width, height } = fitTicket(source.width, source.height);
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext('2d');
  // JPEG has no alpha: a transparent PNG would turn black.
  context.fillStyle = '#fff';
  context.fillRect(0, 0, width, height);
  // A strong reduction (a 4000 px photo to 1200) keeps small print legible.
  context.imageSmoothingQuality = 'high';
  context.drawImage(image, 0, 0, width, height);
  image.close?.();
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error('jpeg'))), 'image/jpeg', 0.85);
  });
}

/** The blob as base64, without any `data:` prefix. */
export function toBase64(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).slice(String(reader.result).indexOf(',') + 1));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(blob);
  });
}
