// Native and bridge mode names share one presentation contract.
const MODES = {
  froid: { label: 'commun.clim.mode.froid', icon: 'snowflake', tone: 'cool' },
  chaud: { label: 'commun.clim.mode.chaud', icon: 'fire', tone: 'warm' },
  auto: { label: 'commun.clim.mode.auto', icon: 'air-conditioner', tone: 'good' },
  déshumidification: { label: 'commun.clim.mode.deshumidification', icon: 'humidity', tone: 'cool' },
  ventilation: { label: 'commun.clim.mode.ventilation', icon: 'fan', tone: 'good' },
  off: { label: 'commun.clim.eteinte', icon: 'power', tone: 'good' },
  aux_heat: { label: 'commun.clim.mode.auxiliaire', icon: 'fire', tone: 'warm' },
};
const ALIASES = { cool: MODES.froid, heat: MODES.chaud, dry: MODES.déshumidification, fan: MODES.ventilation, fan_only: MODES.ventilation, heat_cool: MODES.auto };

export function climateMode(key) {
  if (Object.hasOwn(MODES, key)) return MODES[key];
  return Object.hasOwn(ALIASES, key) ? ALIASES[key] : null;
}
