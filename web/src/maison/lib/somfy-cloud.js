// Private observation state belongs to one mounted card. Never persist it in browser storage.
const ROOT = '/api/integrations/overkiz-cloud';
export const collections = ['scenarios', 'calendar_days', 'calendar_rules', 'triggers', 'schedule', 'delayed_schedule'];
export function safeDate(value, locale, now = Date.now()) {
  if (!Number.isSafeInteger(value) || value < 0 || value > now || value > 8640000000000000) return null;
  return new Date(value).toLocaleString(locale);
}
export function retryDate(attempt, seconds, locale) {
  if (!Number.isSafeInteger(attempt) || attempt < 0 || attempt > Date.now() || !Number.isSafeInteger(seconds) || seconds < 0) return null;
  const value = attempt + seconds * 1000;
  if (!Number.isSafeInteger(value) || value > 8640000000000000) return null;
  return new Date(value).toLocaleString(locale);
}
export function requestFinished(row, id) {
  const s = row?.state;
  if (!s || s.request_id !== id || s.phase === 'persistence_pending') return false;
  // The vault may contain Ready before the controller has acknowledged/published it.
  return s.phase !== 'ready' || row.online === true;
}
function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => { clearTimeout(timer); reject(new DOMException('Aborted', 'AbortError')); };
    const timer = setTimeout(() => { signal.removeEventListener('abort', abort); resolve(); }, ms);
    signal.addEventListener('abort', abort, { once: true });
    if (signal.aborted) abort();
  });
}
export function createCloudObserver(change, transport = fetch, wait = pause) {
  const lifetime = new AbortController();
  let active = true;
  let selection = 0;
  let listEpoch = 0;
  let sending = false;
  let view = { auth: null, instances: [], selected: '', loading: true, pending: null, error: '', catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false };
  const emit = (patch) => { if (active) { view = { ...view, ...patch }; change(view); } };
  const path = () => `${ROOT}/${encodeURIComponent(view.selected)}`;
  async function call(url, options = {}) {
    const response = await transport(url, { cache: 'no-store', signal: lifetime.signal, ...options });
    if (!active) throw new DOMException('Aborted', 'AbortError');
    if (response.status === 403) {
      const body = await response.json().catch(() => ({}));
      emit({ auth: body.auth_policy === 'pin_fallback' ? 'pin_fallback' : 'owner_account', instances: [], selected: '', catalog: null, catalogText: '', details: {} });
      throw new Error('owner');
    }
    if (!response.ok) throw new Error(response.status === 409 ? 'changed_or_busy' : 'unavailable');
    return response;
  }
  function failed(error) { if (active && error.name !== 'AbortError') emit({ error: error.message === 'owner' ? '' : error.message === 'changed_or_busy' ? 'changed_or_busy' : 'unavailable' }); }
  async function list(signal = lifetime.signal) {
    const epoch = ++listEpoch;
    const body = await (await call(ROOT, { signal })).json();
    if (!active || epoch !== listEpoch) return;
    const instances = body.instances ?? [];
    const selected = instances.some(i => i.instance === view.selected) ? view.selected : (instances[0]?.instance ?? '');
    const oldState = view.instances.find(i => i.instance === view.selected)?.state;
    const nextState = instances.find(i => i.instance === selected)?.state;
    const identityChanged = view.selected !== selected || oldState?.generation !== nextState?.generation || ['site_oid', 'gateway_id', 'region'].some(key => oldState?.scope?.[key] !== nextState?.scope?.[key]);
    if (identityChanged) {
      selection++;
      emit({ catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false });
    }
    emit({ instances, selected, auth: 'owner' });
  }
  async function load() {
    emit({ loading: true, error: '' });
    try { await list(); } catch (e) { failed(e); }
    finally { emit({ loading: false }); }
  }
  async function catalog(signal = lifetime.signal) {
    if (view.auth !== 'owner' || !view.selected || view.reading) return;
    const epoch = selection;
    emit({ reading: true, error: '', details: {} });
    try {
      const text = await (await call(`${path()}/catalog`, { signal })).text();
      const data = JSON.parse(text); // Metadata only; exact response text remains the export.
      if (epoch === selection) emit({ catalog: data, catalogText: text });
    } catch (e) { if (epoch === selection) { emit({ catalog: null, catalogText: '' }); failed(e); } }
    finally { if (epoch === selection) emit({ reading: false }); }
  }
  async function definition(collection, index) {
    if (!view.catalog || view.detailBusy) return;
    const epoch = selection;
    const revision = view.catalog.revision;
    const key = `${collection}:${index}`;
    emit({ detailBusy: key, error: '' });
    try {
      const text = await (await call(`${path()}/definitions/${collection}/${index}?revision=${encodeURIComponent(revision)}`)).text();
      if (epoch === selection && view?.catalog?.revision === revision) emit({ details: { ...view.details, [key]: text } });
    } catch (e) { if (epoch === selection) failed(e); }
    finally { if (epoch === selection) emit({ detailBusy: '' }); }
  }
  async function send(input) {
    if (sending || view.auth !== 'owner' || !view.selected) { if ('password' in input) input.password = ''; return; }
    sending = true;
    const deadline = new AbortController();
    const cancel = () => deadline.abort();
    lifetime.signal.addEventListener('abort', cancel, { once: true });
    const timer = setTimeout(cancel, 120000);
    emit({ pending: 'dispatch', error: '' });
    try {
      // Nothing secret enters a PIN continuation, a timer, or the observable view.
      const request = call(`${path()}/requests`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' }, body: JSON.stringify(input), signal: deadline.signal });
      if ('password' in input) input.password = '';
      const { request_id: id } = await (await request).json();
      emit({ pending: id, catalog: null, catalogText: '', details: {} });
      for (let attempt = 0; attempt < 60 && active; attempt++) {
        await list(deadline.signal);
        if (requestFinished(view.instances.find(i => i.instance === view.selected), id)) {
          if (view.instances.find(i => i.instance === view.selected)?.state?.scope) await catalog(deadline.signal);
          return;
        }
        await wait(2000, deadline.signal);
      }
      if (active) emit({ error: 'pending_timeout' });
    } catch (e) { failed(e); }
    finally {
      clearTimeout(timer);
      lifetime.signal.removeEventListener('abort', cancel);
      if (active && deadline.signal.aborted) emit({ error: 'pending_timeout' });
      if ('password' in input) input.password = '';
      sending = false;
      emit({ pending: null });
    }
  }
  function select(instance) {
    if (sending || !view.instances.some(i => i.instance === instance)) return;
    selection++;
    emit({ selected: instance, catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false, error: '' });
  }
  return { load, catalog, definition, send, select, stop() { active = false; lifetime.abort(); view = null; } };
}
