// Synthetic development device; run only through run.sh's network namespace.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { ServerNode, Endpoint, Environment, VendorId } from '@matter/main';
import { ExtendedColorLightDevice } from '@matter/main/devices/extended-color-light';
import { WindowCoveringDevice } from '@matter/main/devices/window-covering';
import { ThermostatDevice } from '@matter/main/devices/thermostat';
import { DoorLockDevice } from '@matter/main/devices/door-lock';
import { WindowCoveringServer } from '@matter/main/behaviors/window-covering';
import { ThermostatServer } from '@matter/main/behaviors/thermostat';
import { DoorLockServer } from '@matter/main/behaviors/door-lock';
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
// Persist only independently observed accessory events/commands, never Moli responses.
const observed = {commands:[], states:{}, events:[]};
const manifest = {};
const policy = {};
let policyTimer;
function record(endpoint, command, fields = {}) {
 observed.commands.push({endpoint, command, fields});
 writeFileSync(`${dir}/extended-state.json`, JSON.stringify(observed));
}
function observe(endpoint, cluster, keys) {
 manifest[endpoint.id] = endpoint.number;
 for (const key of keys) endpoint.events[cluster][`${key}$Changed`].on(value => {
  observed.states[`${endpoint.id}.${key}`] = value;
  observed.events.push({endpoint:endpoint.id,key,value});
  writeFileSync(`${dir}/extended-state.json`, JSON.stringify(observed));
 });
}
if (process.env.E2E_EXTENDED === '1') {
 const color = new Endpoint(ExtendedColorLightDevice, {id:'color', onOff:{onOff:true},
  levelControl:{currentLevel:127}, colorControl:{currentX:20000,currentY:20000,
   colorMode:2,enhancedColorMode:2,coupleColorTempToLevelMinMireds:153,colorTemperatureMireds:250,colorTempPhysicalMinMireds:153,colorTempPhysicalMaxMireds:500}});
 await node.add(color);
 observe(color,'levelControl',['currentLevel']);
 observe(color,'colorControl',['currentX','currentY','colorTemperatureMireds','colorMode']);
 class Cover extends WindowCoveringServer.with('Lift','PositionAwareLift') {
  upOrOpen() { record(this.endpoint.id,'open'); return super.upOrOpen(); }
  downOrClose() { record(this.endpoint.id,'close'); return super.downOrClose(); }
  stopMotion() { record(this.endpoint.id,'stop'); return super.stopMotion(); }
  goToLiftPercentage(fields) { record(this.endpoint.id,'position',fields); return super.goToLiftPercentage(fields); }
 }
 const cover = new Endpoint(WindowCoveringDevice.with(Cover), {id:'cover', windowCovering:{
  type:0, endProductType:0, currentPositionLiftPercent100ths:5000}});
 await node.add(cover);
 observe(cover,'windowCovering',['currentPositionLiftPercent100ths']);
 const thermostat = new Endpoint(ThermostatDevice.with(ThermostatServer.with('Heating','Cooling','AutoMode')), {
  id:'thermostat', thermostat:{absMinHeatSetpointLimit:700,absMaxHeatSetpointLimit:3000,
   absMinCoolSetpointLimit:1600,absMaxCoolSetpointLimit:3200,localTemperature:2000,occupiedHeatingSetpoint:2000,
   occupiedCoolingSetpoint:2600,minHeatSetpointLimit:1000,maxHeatSetpointLimit:2800,
   minCoolSetpointLimit:1800,maxCoolSetpointLimit:3200,minSetpointDeadBand:25,
   controlSequenceOfOperation:4,systemMode:0}});
 await node.add(thermostat);
 observe(thermostat,'thermostat',['occupiedHeatingSetpoint','occupiedCoolingSetpoint','systemMode']);
 // Enable the optional policy attribute honestly; no credentials are configured or needed.
 class Lock extends DoorLockServer.with('PinCredential','CredentialOverTheAirAccess','User') {
  handleLockOperation(operation) { record(this.endpoint.id,operation.actuation); }
 }
 for (const [id, enabled] of [['lock',true],['readonly-lock',false]]) {
  const lock = new Endpoint(DoorLockDevice.with(Lock), {id, doorLock:{
   lockState:1,lockType:0,actuatorEnabled:enabled,operatingMode:0,
   requirePinForRemoteOperation:false,wrongCodeEntryLimit:3,userCodeTemporaryDisableTime:10,
   supportedOperatingModes:{normal:false,vacation:false,privacy:false,passage:false,noRemoteLockUnlock:true,alwaysSet:2047}}});
  await node.add(lock);
  const state = lock.state.doorLock;
  policy[id] = {actuatorEnabled:state.actuatorEnabled,operatingMode:state.operatingMode,
   supportedOperatingModes:state.supportedOperatingModes,requirePinForRemoteOperation:state.requirePinForRemoteOperation};
  observe(lock,'doorLock',['lockState','requirePinForRemoteOperation']);
  if (id === 'lock') {
   // Disposable namespace-local IPC mutates only the real accessory's attribute.
   let applied = 0, busy = false;
   policyTimer = setInterval(async () => {
    if (busy) return;
    let request;
    try { request = JSON.parse(readFileSync(`${dir}/policy-request.json`, 'utf8')); }
    catch (error) { if (error.code === 'ENOENT') return; throw error; }
    if (request.seq === applied) return;
    if (!Number.isSafeInteger(request.seq) || request.seq < 1 || typeof request.requirePin !== 'boolean')
     throw Error('Invalid synthetic policy request');
    busy = true;
    try {
     await lock.set({doorLock:{requirePinForRemoteOperation:request.requirePin}});
     applied = request.seq;
     writeFileSync(`${dir}/policy-reply.json`, JSON.stringify({seq:applied,
      requirePin:lock.state.doorLock.requirePinForRemoteOperation}));
    } finally { busy = false; }
   }, 100);
  }
 }
 writeFileSync(`${dir}/extended-state.json`, JSON.stringify(observed));
}
const light = new Endpoint(OnOffLightDevice, {id:'light', onOff:{onOff:false}});
await node.add(light);
light.events.onOff.onOff$Changed.on(value => writeFileSync(`${dir}/accessory-state.json`, JSON.stringify({on:value})));
await node.start();
writeFileSync(`${dir}/pairing-code`, node.state.commissioning.pairingCodes.manualPairingCode, {mode:0o600});
manifest.light = light.number;
writeFileSync(`${dir}/endpoints.json`, JSON.stringify(manifest));
writeFileSync(`${dir}/accessory-metadata.json`, JSON.stringify(policy,null,2));
writeFileSync(`${dir}/accessory-ready`, 'ready');
for (const sig of ['SIGTERM','SIGINT']) process.on(sig, async()=>{clearInterval(policyTimer); await node.close(); process.exit(0)});
