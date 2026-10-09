// The solar demo's own icons (Material Design Icons, like the rest): kept
// here so the shared set (lib/icons.js) stays as it is.
import {
  mdiSolarPowerVariant,
  mdiTransmissionTower,
  mdiHomeLightningBoltOutline,
  mdiBatteryHigh,
  mdiWaterBoiler,
  mdiHeatPumpOutline,
  mdiCarElectricOutline,
  mdiWeatherSunny,
  mdiWeatherPartlyCloudy,
  mdiWeatherCloudy,
  mdiFlaskOutline,
  mdiTuneVariant,
  mdiPiggyBankOutline,
  mdiLightbulbOnOutline,
  mdiCalendarMonthOutline,
  mdiChartBellCurveCumulative,
  mdiRobotOutline,
} from '@mdi/js';
import { icon } from '../../lib/icons.js';

const own = {
  solar: mdiSolarPowerVariant,
  grid: mdiTransmissionTower,
  house: mdiHomeLightningBoltOutline,
  battery: mdiBatteryHigh,
  'water-boiler': mdiWaterBoiler,
  'heat-pump': mdiHeatPumpOutline,
  car: mdiCarElectricOutline,
  clair: mdiWeatherSunny,
  voile: mdiWeatherPartlyCloudy,
  couvert: mdiWeatherCloudy,
  flask: mdiFlaskOutline,
  tune: mdiTuneVariant,
  piggy: mdiPiggyBankOutline,
  idea: mdiLightbulbOnOutline,
  month: mdiCalendarMonthOutline,
  curve: mdiChartBellCurveCumulative,
  robot: mdiRobotOutline,
};

/** An SVG path by name: ours first, then the shared set. */
export const ico = (name) => own[name] ?? icon(name);
