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
  let disconnecting = false;
  let view = { auth: null, instances: [], selected: '', loading: true, pending: null, error: '', notice: '', catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false };
  const emit = (patch) => { if (active) { view = { ...view, ...patch }; change(view); } };
  const path = () => `${ROOT}/${encodeURIComponent(view.selected)}`;
  function requestDeadline(ms) {
    const controller = new AbortController();
    const cancel = () => controller.abort();
    lifetime.signal.addEventListener('abort', cancel, { once: true });
    const timer = setTimeout(cancel, ms);
    return { signal: controller.signal, dispose() {
      clearTimeout(timer);
      lifetime.signal.removeEventListener('abort', cancel);
    } };
  }
  function retiredCatalog(catalog) {
    return catalog ? { ...catalog, execution: null, freshness: { ...catalog.freshness, current: false } } : null;
  }
  function retireExecution() { emit({ catalog: retiredCatalog(view.catalog) }); }
  async function call(url, options = {}, policyChange = false) {
    const instance = view.selected;
    const response = await transport(url, { cache: 'no-store', signal: lifetime.signal, ...options });
    if (!active) throw new DOMException('Aborted', 'AbortError');
    if (response.ok) return response;
    const body = await response.json().catch(() => ({}));
    const code = body.code ?? body.error;
    if (response.status === 503 && code === 'cache_unavailable' && body.credentials_removed === true) {
      if (instance === view.selected) {
        selection++; listEpoch++;
        clearConnection(instance, false);
        emit({ reading: false, detailBusy: '' });
      }
      throw new Error('cache_unavailable');
    }
    if (policyChange) {
      const codes = new Set(['invalid_pin', 'pin_disabled', 'pin_rate_limited', 'catalog_changed', 'global_read_only']);
      if (codes.has(code)) {
        if (code === 'catalog_changed' || code === 'global_read_only') retireExecution();
        throw new Error(code);
      }
    }
    if (response.status === 403) {
      emit({ auth: body.auth_policy === 'pin_fallback' ? 'pin_fallback' : 'owner_account', instances: [], selected: '', catalog: null, catalogText: '', details: {} });
      throw new Error('owner');
    }
    if (response.status === 409) retireExecution();
    throw new Error(response.status === 409 ? 'changed_or_busy' : 'unavailable');
  }
  function failed(error) {
    const known = new Set(['changed_or_busy', 'invalid_pin', 'pin_disabled', 'pin_rate_limited', 'catalog_changed', 'global_read_only', 'permission_failed', 'cache_unavailable']);
    if (active && error.name !== 'AbortError') emit({ error: error.message === 'owner' ? '' : known.has(error.message) ? error.message : 'unavailable' });
  }
  async function list(signal = lifetime.signal) {
    const epoch = ++listEpoch;
    const body = await (await call(ROOT, { signal })).json();
    if (!active || epoch !== listEpoch) return;
    const instances = body.instances ?? [];
    const selected = instances.some(i => i.instance === view.selected) ? view.selected : (instances[0]?.instance ?? '');
    const oldState = view.instances.find(i => i.instance === view.selected)?.state;
    const nextRow = instances.find(i => i.instance === selected);
    const nextState = nextRow?.state;
    const identityChanged = view.selected !== selected || oldState?.generation !== nextState?.generation || ['site_oid', 'gateway_id', 'region'].some(key => oldState?.scope?.[key] !== nextState?.scope?.[key]);
    if (identityChanged || nextRow?.cleanup_pending) {
      selection++;
      emit({ catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false });
    } else if (nextRow?.configured === false || nextRow?.online !== true) {
      retireExecution();
    }
    emit({ instances, selected, auth: 'owner' });
  }
  async function load() {
    if (disconnecting) return false;
    emit({ loading: true, error: '', notice: '' });
    try { await list(); return active && view.auth === 'owner'; } catch (e) { failed(e); return false; }
    finally { emit({ loading: false }); }
  }
  async function open() {
    const loaded = await load();
    const row = loaded && active ? view.instances.find(i => i.instance === view.selected) : null;
    if (row?.configured !== false && !row?.cleanup_pending && row?.state?.scope) await catalog();
  }
  async function catalog(signal = lifetime.signal) {
    if (disconnecting || view.auth !== 'owner' || !view.selected || view.reading) return;
    const epoch = selection;
    emit({ reading: true, error: '', details: {} });
    try {
      const text = await (await call(`${path()}/catalog`, { signal })).text();
      const data = JSON.parse(text); // Metadata only; exact response text remains the export.
      if (epoch === selection) {
        const row = view.instances.find(i => i.instance === view.selected);
        const current = row?.configured !== false && !row?.cleanup_pending && row?.online === true
          && data.freshness?.current === true;
        const qualified = current ? data : retiredCatalog(data);
        // An unavailable scenario collection does not retire fresh calendar data.
        emit({ catalog: current && data.freshness?.collections?.scenarios !== true
          ? { ...qualified, execution: null } : qualified, catalogText: text });
      }
    } catch (e) { if (epoch === selection) { emit({ catalog: null, catalogText: '' }); failed(e); } }
    finally { if (epoch === selection) emit({ reading: false }); }
  }
  async function definition(collection, index) {
    if (disconnecting || !view.catalog || view.detailBusy) return;
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
  async function dispatch(input, endpoint) {
    const row = view.instances.find(i => i.instance === view.selected);
    if (disconnecting || sending || view.auth !== 'owner' || !view.selected || row?.configured === false || row?.cleanup_pending) { if ('password' in input) input.password = ''; if ('pin' in input) input.pin = ''; return; }
    sending = true;
    const deadline = requestDeadline(120000);
    emit({ pending: 'dispatch', error: '', notice: '' });
    try {
      // Nothing secret enters a PIN continuation, a timer, or the observable view.
      const request = call(`${path()}/${endpoint}`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' }, body: JSON.stringify(input), signal: deadline.signal }, endpoint === 'scenario-permission');
      if ('password' in input) input.password = ''; if ('pin' in input) input.pin = '';
      const { request_id: id } = await (await request).json();
      emit({ pending: id, catalog: null, catalogText: '', details: {} });
      for (let attempt = 0; attempt < 60 && active; attempt++) {
        await list(deadline.signal);
        if (requestFinished(view.instances.find(i => i.instance === view.selected), id)) {
          if (endpoint === 'scenario-permission' && view.instances.find(i => i.instance === view.selected)?.state?.failure) throw new Error('permission_failed');
          if (view.instances.find(i => i.instance === view.selected)?.state?.scope) await catalog(deadline.signal);
          return;
        }
        await wait(2000, deadline.signal);
      }
      if (active) emit({ error: 'pending_timeout' });
    } catch (e) { failed(e); }
    finally {
      deadline.dispose();
      if (active && deadline.signal.aborted) emit({ error: 'pending_timeout' });
      if ('password' in input) input.password = ''; if ('pin' in input) input.pin = '';
      sending = false;
      emit({ pending: null });
    }
  }
  function send(input) { return dispatch(input, 'requests'); }
  async function permission(input) {
    await dispatch(input, 'scenario-permission');
    return active ? view.error : 'unavailable';
  }
  function clearConnection(instance, complete) {
    const instances = view.instances.flatMap(row => row.instance !== instance ? [row]
      : complete && row.configured === false ? [] : [{ ...row, online: false, session_phase: null, cleanup_pending: !complete, state: { phase: 'disconnected' } }]);
    emit({ instances, selected: instances.some(row => row.instance === instance) ? instance : '', catalog: null, catalogText: '', details: {}, notice: complete ? 'disconnected' : '' });
  }
  async function disconnect() {
    if (sending || view.auth !== 'owner' || !view.selected) return;
    sending = true;
    disconnecting = true;
    const deadline = requestDeadline(30000);
    const instance = view.selected;
    // In-flight reads belong to the old account view, including on a failed purge.
    selection++;
    listEpoch++;
    retireExecution();
    emit({ pending: 'disconnect', error: '', notice: '', reading: false, detailBusy: '' });
    try {
      const response = await call(path(), { method: 'DELETE', headers: { 'x-moli-origin': 'ui' }, signal: deadline.signal });
      if ((await response.json()).disconnected !== true) throw new Error('unavailable');
      if (!active) return;
      // Only an acknowledged durable purge clears the private browser copy.
      clearConnection(instance, true);
      await list(deadline.signal);
    } catch (e) {
      if (active && e.message === 'cache_unavailable') clearConnection(instance, false);
      failed(e);
    }
    finally {
      deadline.dispose();
      if (active && deadline.signal.aborted) emit({ error: 'pending_timeout' });
      disconnecting = false;
      sending = false;
      emit({ pending: null });
    }
  }
  function select(instance) {
    if (sending || !view.instances.some(i => i.instance === instance)) return;
    selection++;
    emit({ selected: instance, catalog: null, catalogText: '', details: {}, detailBusy: '', reading: false, error: '', notice: '' });
  }
  return { load, open, catalog, definition, send, permission, disconnect, select, stop() { active = false; lifetime.abort(); view = null; } };
}
