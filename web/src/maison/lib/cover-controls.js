import { coverOrder, enumChoice, numericRange, positionPoint } from './capabilities.js';

const COMMANDS = ['open', 'stop', 'close', 'cycle'];

/** Speed describes the next intention, never an observed or persistent setting. */
export function coverCommands(device, speed = 'normal') {
  const order = coverOrder(device, true);
  const choices = order?.kind?.values;
  return COMMANDS.flatMap((command) => {
    const directional = command === 'open' || command === 'close';
    const normal = enumChoice(choices, command);
    const slow = directional ? enumChoice(choices, `${command}_slow`) : null;
    if (!normal && !slow) return [];
    return [{ command, key: order.key, value: speed === 'slow' && directional ? slow : normal }];
  });
}

/** The optional slow point is an explicit write-only action, not a second reading. */
export function coverPositionTarget(device, speed = 'normal') {
  const candidates = device?.points?.filter((point) => point.key === 'percent_control_slow') ?? [];
  const point = speed === 'slow'
    ? candidates.length === 1 && candidates[0].access?.read === false && candidates[0].unit === '%' ? candidates[0] : null
    : positionPoint(device)?.semantic === 'position' ? positionPoint(device) : null;
  const range = point ? numericRange(device, point.key, 1, { min: 0, max: 100 }) : null;
  return range && range.min >= 0 && range.max <= 100 ? { point, range } : null;
}

export function hasSlowCoverControl(device) {
  return coverCommands(device, 'slow').some(({ command, value }) => ['open', 'close'].includes(command) && value)
    || !!coverPositionTarget(device, 'slow');
}
