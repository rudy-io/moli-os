// The colours of a chart's series: muted, flat tones (rose, ochre, slate
// blue, teal…), in an order where each colour stays distinct from the next
// one, colour-blind eyes included (checked with a palette validator, light
// and dark themes: neighbours ΔE ≥ 9.5 under protanopia, ≥ 16 in full
// colour). Slots are handed out in this order, never cycled past the end:
// beyond ten series, the rest share the neutral colour.

export const SERIES = ['#d65f86', '#c08a14', '#4f6fbf', '#14a088', '#b04a36', '#3d8fd9', '#878c1c', '#7466d8', '#c4509e', '#4f9a52'];

/** What no series explains (« le reste »), and series past the tenth. */
export const NEUTRAL = '#9b988f';

/** The colour of the series ranked `i` (0 = the first slot). */
export const seriesColor = (i) => SERIES[i] ?? NEUTRAL;
