// Asking the house, the one way: as the dashboard (`x-moli-origin: ui`),
// JSON in and out, the house's own words when it refuses.

/** What the house refused: its words (`message`), `status` and answer. */
export class Refused extends Error {
  constructor(status, body, fallback) {
    super(body?.error ?? fallback);
    this.status = status;
    this.body = body;
  }
}

/** Sends `body` (JSON, or as is with `type`: a file, a recording) and
 *  returns the answer (`{}` when there is none). A refusal throws
 *  `Refused`; a 202 (an order held by the guard) is an answer. */
export async function request(method, url, body, { type } = {}) {
  const headers = { 'x-moli-origin': 'ui' };
  if (type) headers['content-type'] = type;
  else if (body !== undefined) {
    headers['content-type'] = 'application/json';
    body = JSON.stringify(body);
  }
  const res = await fetch(url, { method, headers, body });
  const answer = await res.json().catch(() => ({}));
  if (!res.ok) throw new Refused(res.status, answer, res.statusText || String(res.status));
  return answer;
}
