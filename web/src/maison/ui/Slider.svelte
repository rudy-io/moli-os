<script>
  /**
   * A range slider that stays smooth under the finger: while it is held, nothing
   * reactive changes (the fill is painted straight on the element), and the
   * value is only reported on release. A new `value` from outside is applied
   * once the slider is let go, and again on release (a lamp that rounds or
   * refuses the value puts the slider back where the lamp is).
   *
   * tone: « level » fills up to the thumb; « kelvin » shows warm to cold whites.
   * valuetext: the value in words for screen readers (« 2700 K, warm white »).
   */
  let { min, max, step = 1, value, label, tone = 'level', valuetext = null, onlive = null, oncommit } = $props();

  let el = $state();
  let held = $state(false);
  // The value under the finger when it came down: a release without a move
  // sends no `change`, so the slider is let go there.
  let start = null;

  const paint = () => {
    if (!el) return;
    el.style.setProperty('--p', `${max > min ? ((Number(el.value) - min) / (max - min)) * 100 : 100}%`);
    if (valuetext) el.setAttribute('aria-valuetext', valuetext(Number(el.value)));
  };

  $effect(() => {
    if (el && !held) {
      el.value = String(value);
      paint();
    }
  });

  function live() {
    paint();
    onlive?.(Number(el.value));
  }

  function commit() {
    held = false;
    oncommit(Number(el.value));
  }

  function press() {
    held = true;
    start = el.value;
  }

  // Released (or the pointer lost): when the value moved, `change` follows and
  // lets go; when it did not, nothing follows, so let go here.
  function release() {
    if (held && el.value === start) held = false;
  }
</script>

<input
  bind:this={el}
  class="slider {tone}"
  type="range"
  {min}
  {max}
  {step}
  aria-label={label}
  onpointerdown={press}
  onpointerup={release}
  onlostpointercapture={release}
  onpointercancel={() => (held = false)}
  oninput={live}
  onchange={commit} />

<style>
  .slider {
    appearance: none;
    -webkit-appearance: none;
    flex: 1;
    min-width: 0;
    width: 100%;
    height: 6px;
    margin: 14px 0;
    border-radius: 999px;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--warm) 65%, #fff),
      var(--warm) var(--p, 50%),
      var(--surface-3) var(--p, 50%)
    );
    box-shadow: inset 0 1px 2px rgb(0 0 0 / 12%);
    outline: none;
    cursor: pointer;
    touch-action: pan-y;
  }

  .slider.kelvin {
    background: linear-gradient(90deg, #ffb35c, #ffe9d2 45%, #e9f0ff 70%, #bcd2ff);
  }

  /* A small thumb that swells when it is held, hovered or focused. */
  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 14px;
    height: 14px;
    border: 0;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgb(0 0 0 / 35%), 0 0 0 1px rgb(0 0 0 / 8%);
    transition: transform 0.15s var(--ease);
  }

  .slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border: 0;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgb(0 0 0 / 35%), 0 0 0 1px rgb(0 0 0 / 8%);
    transition: transform 0.15s var(--ease);
  }

  .slider:hover::-webkit-slider-thumb,
  .slider:focus-visible::-webkit-slider-thumb,
  .slider:active::-webkit-slider-thumb {
    transform: scale(1.7);
  }

  .slider:hover::-moz-range-thumb,
  .slider:focus-visible::-moz-range-thumb,
  .slider:active::-moz-range-thumb {
    transform: scale(1.7);
  }

  .slider::-moz-range-track {
    background: transparent;
  }

  .slider:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 6px;
  }

  /* One rule per engine: a selector list holding the other engine's
     pseudo-element is dropped whole. */
  @media (prefers-reduced-motion: reduce) {
    .slider::-webkit-slider-thumb {
      transition: none;
    }

    .slider::-moz-range-thumb {
      transition: none;
    }
  }
</style>
