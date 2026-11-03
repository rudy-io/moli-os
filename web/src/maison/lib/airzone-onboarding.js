// A proposal is text for a human installation; it never applies configuration.
export function gatewayProposal({ id, host, port }, drivers = []) {
  id = String(id ?? '').trim(); host = String(host ?? '').trim(); port = String(port ?? '').trim();
  if (!/^[a-z][a-z0-9_-]{0,31}$/.test(id) || ['core','assistant','automations','energie','charges','lumieres','personnes'].includes(id)) return {error:'id'};
  if (drivers.some((driver) => driver.instance === id)) return {error:'duplicate'};
  const ipv4 = /^\d+(?:\.\d+){3}$/.test(host);
  const validHost = host.length > 0 && host.length <= 253 && (ipv4
    ? host.split('.').every((part) => /^(?:0|[1-9]\d{0,2})$/.test(part) && Number(part) <= 255)
    : host.split('.').every((part) => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/i.test(part)));
  if (!validHost) return {error:'host'};
  if (!/^\d{1,5}$/.test(port) || Number(port) < 1 || Number(port) > 65535) return {error:'port'};
  return {error:null, config:`[[driver]]\nid = "${id}"\nkind = "airzone"\n\n[driver.options]\nhost = "${host}"\nport = ${Number(port)}\npoll_secs = 30\nread_only = true\n`};
}
