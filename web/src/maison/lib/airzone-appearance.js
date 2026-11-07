import {climateMode} from './climate.js';

/** Mode is a setting, demand is an observation. Neither a temperature gap nor
 * retained/offline data can turn a zone into an active coloured thermostat. */
export function zoneAppearance(summary,sync) {
  const mode=climateMode(summary?.mode);
  const live=summary?.online===true&&sync?.state==='current';
  const enabled=live&&summary.on===true&&mode!==null&&mode.icon!=='power';
  const tone=!enabled?'neutral':['warm','cool'].includes(mode.tone)?mode.tone:mode.icon==='fan'?'good':'neutral';
  const icon=!live?(summary?.availability==='offline'||sync?.state==='disconnected'?'offline':'clock')
    :summary.on===false||mode?.icon==='power'?'power':mode?.icon??'air-conditioner';
  return {mode,tone,icon,live,demand:enabled&&summary.status==='demand'};
}
