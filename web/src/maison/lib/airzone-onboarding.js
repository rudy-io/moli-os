const RESERVED = new Set(['core','assistant','automations','energie','charges','lumieres','personnes']);
const INSTANCE = /^[a-z][a-z0-9_-]{0,31}$/;
const MAC = /^[0-9a-f]{12}$/;
const ERRORS = new Set(['invalid_instance','invalid_host','invalid_port','invalid_fingerprint','expected_identity_required','invalid_identity','duplicate_instance','duplicate_gateway','identity_unavailable','identity_changed','probe_failed','busy','limit_reached','persistence_failed','unavailable','shutting_down','not_found','instance_mismatch','gateway_disabled','mode_conflict']);

export function nextGatewayInstance(drivers = []) {
  const used = new Set(drivers.map((driver) => driver.instance));
  let number = 2;
  while (used.has(`airzone-${number}`)) number++;
  return `airzone-${number}`;
}

// A small UI contract, not arbitrary driver options. The server validates again.
function existingInstance(instance) {
  return typeof instance === 'string' && instance.length > 0 && !/[:/]/.test(instance) && !RESERVED.has(instance);
}

// Existing configuration IDs predate the onboarding syntax; never trim or rename them.
export function gatewayRequest(input, { existing = false } = {}) {
  const instance = existing ? input?.instance : String(input?.instance ?? '').trim();
  const host = String(input?.host ?? '').trim();
  const port = String(input?.port ?? '').trim();
  if (existing ? !existingInstance(instance) : !INSTANCE.test(instance) || RESERVED.has(instance)) return {error:'invalid_instance'};
  if (existing && (instance === '.' || instance === '..')) return {error:'unaddressable_instance'};
  const pieces = host.split('.');
  const octets = pieces.map(Number);
  if (pieces.length !== 4 || pieces.some((part) => !/^(?:0|[1-9]\d{0,2})$/.test(part)) || octets.some((part) => part > 255)
    || !(octets[0] === 10 || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) || (octets[0] === 192 && octets[1] === 168))) return {error:'invalid_host'};
  if (!/^\d{1,5}$/.test(port) || Number(port) < 1 || Number(port) > 65535) return {error:'invalid_port'};
  const request = {instance,host,port:Number(port)};
  if (input.tls_fingerprint !== undefined) {
    const fingerprint = String(input.tls_fingerprint).replace(/[:\s]/g,'').toLowerCase();
    if (String(input.tls_fingerprint).length > 128 || !/^[0-9a-f]{64}$/.test(fingerprint)) return {error:'invalid_fingerprint'};
    request.tls_fingerprint = fingerprint;
  }
  return {error:null,request};
}

function previewIsValid(data, instance) {
  const text = (value) => value === null || (typeof value === 'string' && value.length <= 128 && !/[\u0000-\u001f\u007f]/.test(value));
  return data && data.instance === instance && typeof data.mac === 'string' && MAC.test(data.mac) && data.read_only === true
    && [null,'ws_az','ws_aidoo'].includes(data.gateway_type) && text(data.api_version) && text(data.firmware)
    && Number.isSafeInteger(data.systems) && data.systems >= 0 && data.systems <= 256
    && Number.isSafeInteger(data.zones) && data.zones >= 0 && data.zones <= 4096 && typeof data.incomplete === 'boolean';
}

async function exchange(path, request, validate, fetcher, method = 'POST', signal) {
  let response;
  try {
    response = await fetcher(path, {method,headers:{'content-type':'application/json','x-moli-origin':'ui'},...(method === 'GET' ? {} : {body:JSON.stringify(request)}),...(signal ? {signal} : {})});
  } catch { return {ok:false,error:signal?.aborted ? 'cancelled' : 'network'}; }
  if (response.status === 403) return {ok:false,error:'auth',status:403};
  let data;
  try { data = await response.json(); } catch { data = null; }
  if (!response.ok) {
    const error = ERRORS.has(data?.error) ? data.error : response.status === 429 ? 'busy' : 'server';
    const result = {ok:false,error,status:response.status};
    if (['duplicate_instance','duplicate_gateway'].includes(error) && existingInstance(data?.existing_instance)) result.existingInstance = data.existing_instance;
    return result;
  }
  if (response.status !== 200 || !validate(data)) return {ok:false,error:'response',status:response.status};
  return {ok:true,data};
}

export async function probeGateway(input, fetcher = globalThis.fetch, {signal} = {}) {
  const parsed = gatewayRequest(input);
  if (parsed.error) return {ok:false,error:parsed.error};
  return exchange('/api/integrations/airzone/probe', parsed.request, (data) => previewIsValid(data,parsed.request.instance), fetcher,'POST',signal);
}

function savedIsValid(data, instance, allowExistingMode = false) {
  return data?.instance === instance && data.stored === true && typeof data.already_installed === 'boolean'
    && (data.read_only === true || (allowExistingMode && data.already_installed && data.read_only === false));
}

async function saveGateway(input, expectedMac, updating, fetcher) {
  const parsed = gatewayRequest(input,{existing:updating});
  if (parsed.error) return {ok:false,error:parsed.error};
  if (!expectedMac) return {ok:false,error:'expected_identity_required'};
  if (typeof expectedMac !== 'string' || !MAC.test(expectedMac)) return {ok:false,error:'invalid_identity'};
  const path = '/api/integrations/airzone' + (updating ? `/${encodeURIComponent(parsed.request.instance)}` : '');
  return exchange(path, {...parsed.request,expected_mac:expectedMac}, (data) => savedIsValid(data,parsed.request.instance,!updating), fetcher, updating ? 'PUT' : 'POST');
}

export function addGateway(input, expectedMac, fetcher = globalThis.fetch) {
  return saveGateway(input,expectedMac,false,fetcher);
}

export function updateGateway(input, expectedMac, fetcher = globalThis.fetch) {
  return saveGateway(input,expectedMac,true,fetcher);
}

export async function probeExistingGateway(input, fetcher = globalThis.fetch, {signal} = {}) {
  const parsed = gatewayRequest(input,{existing:true});
  if (parsed.error) return {ok:false,error:parsed.error};
  return exchange(`/api/integrations/airzone/${encodeURIComponent(parsed.request.instance)}/probe`,parsed.request,(data) => previewIsValid(data,parsed.request.instance),fetcher,'POST',signal);
}

function discoveryIsValid(data) {
  if (!Array.isArray(data?.gateways) || data.gateways.length > 32 || typeof data.incomplete !== 'boolean'
    || !Number.isSafeInteger(data.scanned) || !Number.isSafeInteger(data.target_count)
    || data.scanned < data.gateways.length || data.target_count < data.scanned || data.target_count > 512) return false;
  const macs = new Set();
  return data.gateways.every((item) => {
    const parsed = gatewayRequest({...item,instance:'airzone-discovery',...(item?.tls_fingerprint === null ? {tls_fingerprint:undefined} : {})});
    if (parsed.error || !Number.isSafeInteger(item.port) || !(item.tls_fingerprint === null || typeof item.tls_fingerprint === 'string' && /^[0-9a-f]{64}$/.test(item.tls_fingerprint)) || [0,255].includes(Number(item.host.split('.')[3]))
      || !previewIsValid({...item,instance:'airzone-discovery'},'airzone-discovery') || macs.has(item.mac)) return false;
    if (item.existing_instance === null ? item.existing_enabled !== null : !existingInstance(item.existing_instance) || typeof item.existing_enabled !== 'boolean') return false;
    macs.add(item.mac);
    return true;
  });
}

/** One bounded server-side search. Cancellation never installs a gateway. */
export function discoverGateways({signal} = {}, fetcher = globalThis.fetch) {
  return exchange('/api/integrations/airzone/discover',{},discoveryIsValid,fetcher,'POST',signal);
}

export async function withdrawGateway(instance, fetcher = globalThis.fetch) {
  if (!existingInstance(instance)) return {ok:false,error:'invalid_instance'};
  if (instance === '.' || instance === '..') return {ok:false,error:'unaddressable_instance'};
  return exchange(`/api/integrations/airzone/${encodeURIComponent(instance)}/withdraw`,{},(data) => data?.instance === instance && data.stored === true && data.enabled === false && typeof data.read_only === 'boolean',fetcher);
}

/** A separate, PIN-approved permission change; connection editing stays read-only. */
export async function changeGatewayMode(instance, readOnly, expectedReadOnly, fetcher = globalThis.fetch) {
  if (!existingInstance(instance)) return {ok:false,error:'invalid_instance'};
  if (instance === '.' || instance === '..') return {ok:false,error:'unaddressable_instance'};
  if (typeof readOnly !== 'boolean' || typeof expectedReadOnly !== 'boolean') return {ok:false,error:'invalid_mode'};
  return exchange(`/api/integrations/airzone/${encodeURIComponent(instance)}/mode`,
    {read_only:readOnly,expected_read_only:expectedReadOnly},
    (data) => data?.instance === instance && data.stored === true && data.read_only === readOnly && typeof data.running === 'boolean',fetcher);
}

function gatewayIsValid(item) {
  return item && existingInstance(item.instance)
    && typeof item.host === 'string' && item.host.length > 0 && item.host.length <= 253 && !/[\u0000-\u0020\u007f]/.test(item.host)
    && Number.isSafeInteger(item.port) && item.port >= 1 && item.port <= 65535
    && (item.tls_fingerprint === null || (typeof item.tls_fingerprint === 'string' && /^[0-9a-f]{64}$/.test(item.tls_fingerprint)))
    && (item.mac === null || (typeof item.mac === 'string' && MAC.test(item.mac)))
    && typeof item.enabled === 'boolean' && typeof item.read_only === 'boolean' && typeof item.running === 'boolean'
    && (item.enabled || !item.running) && ['configuration','moli'].includes(item.source);
}

export function listGateways(fetcher = globalThis.fetch) {
  return exchange('/api/integrations/airzone',null,(data) => Array.isArray(data?.gateways) && data.gateways.length <= 256
    && data.gateways.every(gatewayIsValid) && new Set(data.gateways.map((item) => item.instance)).size === data.gateways.length,fetcher,'GET');
}
