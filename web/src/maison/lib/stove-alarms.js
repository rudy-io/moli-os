// A pellet stove's past alarms (Micronova), from the history of its state
// (`mode`) and of its alarm code (`alarm`), each a list of [ts, value]
// changes, oldest first. The board keeps the last alarm's code until the
// next lighting: a code alone, the stove reset, is no alarm now.

const ALARM = ['alarm', 'mem_alarm'];
const LIGHTING = ['ignition', 'awaiting_flame', 'starting'];

/** Runs of one list: [{ from, to, value }], `to` null for the last. */
function runs(list) {
  const out = [];
  for (const [ts, value] of list ?? []) {
    const last = out.at(-1);
    if (last && last.value === value) continue;
    if (last) last.to = ts;
    out.push({ from: ts, to: null, value });
  }
  return out;
}

const overlaps = (a, b) => a.from < (b.to ?? Infinity) && b.from < (a.to ?? Infinity);
const isCode = (v) => typeof v === 'string' && v !== '' && v !== 'OK';

/** The stove's alarms, newest first: { start, end (null: not reset yet),
 *  code (null: none read), lighting (the lighting it stopped, or null),
 *  stopped (false: a code said without the stove stopping) }. */
export function alarms(modes, codes) {
  const states = runs(modes);
  const said = runs(codes).filter((r) => isCode(r.value));
  const out = [];
  states.forEach((r, i) => {
    if (!ALARM.includes(r.value) || ALARM.includes(states[i - 1]?.value)) return;
    let to = i;
    while (ALARM.includes(states[to + 1]?.value)) to++;
    const span = { from: r.from, to: states[to].to };
    let first = i;
    while (first > 0 && LIGHTING.includes(states[first - 1].value)) first--;
    const code = said.filter((c) => overlaps(c, span)).at(-1);
    out.push({ start: span.from, end: span.to, code: code?.value ?? null, lighting: first < i ? states[first].from : null, stopped: true });
  });
  for (const c of said) {
    if (out.some((a) => overlaps(c, { from: a.start, to: a.end }))) continue;
    out.push({ start: c.from, end: c.to, code: c.value, lighting: null, stopped: false });
  }
  return out.sort((a, b) => b.start - a.start);
}
