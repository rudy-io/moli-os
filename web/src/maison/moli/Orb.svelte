<script>
  /** Moli's presence: breathes when idle, swirls while thinking, follows
   *  the voice (`level`, 0..1) while listening or speaking. */
  let { size = 40, state = 'idle', level = 0 } = $props();
</script>

<span class="orb {state}" style="--size:{size}px; --level:{level}" aria-hidden="true">
  <i class="core"></i>
  <i class="halo"></i>
</span>

<style>
  .orb {
    position: relative;
    display: inline-block;
    width: var(--size);
    height: var(--size);
    flex: none;
  }

  .core,
  .halo {
    position: absolute;
    inset: 0;
    border-radius: 50%;
  }

  .core {
    background:
      radial-gradient(circle at 34% 28%, rgb(255 255 255 / 70%) 0 14%, transparent 15%),
      conic-gradient(from 200deg, #f5c84b, #f0a53a, #ef7f5a, #8f8cf5, #5fb4f0, #f5c84b);
    animation: breathe 5s ease-in-out infinite;
  }

  .halo {
    inset: -18%;
    background: conic-gradient(from 0deg, #f0a53a55, #8f8cf555, #5fb4f055, #f0a53a55);
    filter: blur(calc(var(--size) / 6));
    opacity: 0.5;
    z-index: -1;
    animation: breathe 5s ease-in-out infinite;
  }

  .thinking .core {
    animation: swirl 1.2s linear infinite;
  }

  .thinking .halo {
    opacity: 0.9;
    animation: swirl 2.4s linear infinite reverse;
  }

  .listening .core,
  .hearing .core {
    animation: none;
    transform: scale(calc(1 + var(--level) * 0.12));
    transition: transform 0.08s linear;
  }

  .listening .halo,
  .hearing .halo {
    opacity: 1;
    animation: listen 0.9s ease-in-out infinite;
  }

  .speaking .core {
    animation: none;
    transform: scale(calc(1 + var(--level) * 0.18));
    transition: transform 0.08s linear;
  }

  .speaking .halo {
    opacity: calc(0.5 + var(--level) * 0.5);
    animation: swirl 3s linear infinite;
  }

  @keyframes breathe {
    50% {
      transform: scale(1.05) rotate(18deg);
      filter: saturate(1.2);
    }
  }

  @keyframes swirl {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes listen {
    50% {
      transform: scale(1.12);
    }
  }
</style>
