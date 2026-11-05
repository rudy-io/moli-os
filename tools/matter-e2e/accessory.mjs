// Synthetic development device; run only through run.sh's network namespace.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { ServerNode, Endpoint, Environment, VendorId } from '@matter/main';
import { OnOffLightDevice } from '@matter/main/devices/on-off-light';
const dir = process.env.E2E_DIR;
if (!dir || process.env.E2E_ISOLATED !== 'yes') throw Error('Use run.sh');
Environment.default.vars.set('storage.path', `${dir}/accessory`);
Environment.default.vars.set('log.level', 'error');
// Extract pinned upstream test public certificates without copying private keys.
const sources = [
 ['paa', 'ChipPAAuthorities.ts', /TestCert_PAA_NoVID_Cert = Bytes.fromHex\(\s*"([0-9a-f]+)"/i],
 ['cd', 'kinds/CertificationDeclaration.ts', /TestCMS_SignerCertificate = Bytes.fromHex\(\s*"([0-9a-f]+)"/i],
];
for (const [kind, file, re] of sources) {
 const source = readFileSync(new URL(`node_modules/@matter/protocol/src/certificate/${file}`, import.meta.url), 'utf8');
 const match = source.match(re); if (!match) throw Error(`Missing pinned ${kind} certificate`);
 mkdirSync(`${dir}/${kind}`, {recursive:true});
 writeFileSync(`${dir}/${kind}/test.der`, Buffer.from(match[1], 'hex'));
}
const node = await ServerNode.create({
 id: 'moli-e2e-accessory', network: {port:5540},
 commissioning: {passcode:20202021, discriminator:3840},
 basicInformation: {vendorId:VendorId(0xfff1), productId:0x8000, vendorName:'Synthetic', productName:'Moli E2E', nodeLabel:'Moli E2E', serialNumber:'E2E-ONLY-001'},
});
const light = new Endpoint(OnOffLightDevice, {id:'light', onOff:{onOff:false}});
await node.add(light);
light.events.onOff.onOff$Changed.on(value => writeFileSync(`${dir}/accessory-state.json`, JSON.stringify({on:value})));
await node.start();
writeFileSync(`${dir}/pairing-code`, node.state.commissioning.pairingCodes.manualPairingCode, {mode:0o600});
writeFileSync(`${dir}/accessory-ready`, 'ready');
for (const sig of ['SIGTERM','SIGINT']) process.on(sig, async()=>{await node.close(); process.exit(0)});
