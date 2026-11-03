const RESERVED = new Set(['core','assistant','automations','energie','charges','lumieres','personnes']);
const INSTANCE = /^[a-z][a-z0-9_-]{0,31}$/;
const MAC = /^[0-9a-f]{12}$/;
const ERRORS = new Set(['invalid_instance','invalid_host','invalid_port','invalid_fingerprint','expected_identity_required','invalid_identity','duplicate_instance','duplicate_gateway','identity_unavailable','identity_changed','probe_failed','busy','limit_reached','persistence_failed','unavailable','shutting_down']);

export function nextGatewayInstance(drivers = []) {
  const used = new Set(drivers.map((driver) => driver.instance));
  let number = 2;
  while (used.has(`airzone-${number}`)) number++;
  return `airzone-${number}`;
}

// A small UI contract, not arbitrary driver options. The server validates again.
export function gatewayRequest(input) {
  const instance = String(input?.instance ?? '').trim();
  const host = String(input?.host ?? '').trim();
  const port = String(input?.port ?? '').trim();
  if (!INSTANCE.test(instance) || RESERVED.has(instance)) return {error:'invalid_instance'};
  const pieces = host.split('.');
  const octets = pieces.map(Number);
  if (pieces.length !== 4 || pieces.some((part) => !/^(?:0|[1-9]\d{0,2})$/.test(part)) || octets.some((part) => part > 255)
    || !(octets[0] === 10 || octets[0] === 127 || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) || (octets[0] === 192 && octets[1] === 168))) return {error:'invalid_host'};
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

async function exchange(path, request, installed, fetcher) {
  let response;
  try {
    response = await fetcher(path, {method:'POST',headers:{'content-type':'application/json','x-moli-origin':'ui'},body:JSON.stringify(request)});
  } catch { return {ok:false,error:'network'}; }
  if (response.status === 403) return {ok:false,error:'auth',status:403};
  let data;
  try { data = await response.json(); } catch { data = null; }
  if (!response.ok) {
    const error = ERRORS.has(data?.error) ? data.error : response.status === 429 ? 'busy' : 'server';
    const result = {ok:false,error,status:response.status};
    if (['duplicate_instance','duplicate_gateway'].includes(error) && typeof data?.existing_instance === 'string' && INSTANCE.test(data.existing_instance)) result.existingInstance = data.existing_instance;
    return result;
  }
  const valid = installed
    ? data?.instance === request.instance && data.stored === true && data.read_only === true && typeof data.already_installed === 'boolean'
    : previewIsValid(data, request.instance);
  if (response.status !== 200 || !valid) return {ok:false,error:'response',status:response.status};
  return {ok:true,data};
}

export async function probeGateway(input, fetcher = globalThis.fetch) {
  const parsed = gatewayRequest(input);
  if (parsed.error) return {ok:false,error:parsed.error};
  return exchange('/api/integrations/airzone/probe', parsed.request, false, fetcher);
}

export async function addGateway(input, expectedMac, fetcher = globalThis.fetch) {
  const parsed = gatewayRequest(input);
  if (parsed.error) return {ok:false,error:parsed.error};
  if (!expectedMac) return {ok:false,error:'expected_identity_required'};
  if (typeof expectedMac !== 'string' || !MAC.test(expectedMac)) return {ok:false,error:'invalid_identity'};
  return exchange('/api/integrations/airzone', {...parsed.request,expected_mac:expectedMac}, true, fetcher);
}
