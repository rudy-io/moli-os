<script>
  import { onDestroy, onMount } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A device's live video, played by this browser (WebRTC): Moli only
   *  passes the offer and the answer (/api/devices/{id}/live); the video
   *  comes straight from the device. So it plays at home, where the
   *  browser reaches the device; elsewhere it says so. */
  let { id } = $props();

  let video = $state(null);
  let status = $state('connecting'); // connecting | playing | failed
  let reason = $state('');
  let pc = null;
  let giveUp = null;

  /** No video within this: the way from the device to this browser is shut
   *  (away from home, a firewall). */
  const PATIENCE = 15_000;

  /** Every candidate in the offer: the device takes one offer, no trickle. */
  function gathered(peer, ms = 3000) {
    if (peer.iceGatheringState === 'complete') return Promise.resolve();
    return new Promise((resolve) => {
      const done = () => peer.iceGatheringState === 'complete' && resolve();
      peer.addEventListener('icegatheringstatechange', done);
      setTimeout(resolve, ms);
    });
  }

  async function start() {
    stop();
    status = 'connecting';
    reason = '';
    try {
      const peer = new RTCPeerConnection();
      pc = peer;
      peer.addTransceiver('video', { direction: 'recvonly' });
      peer.ontrack = (e) => {
        if (video) video.srcObject = e.streams[0] ?? new MediaStream([e.track]);
      };
      const unreachable = () => {
        if (pc !== peer || status === 'playing') return;
        stop();
        status = 'failed';
        reason = t('salon.impression.camera_injoignable');
      };
      peer.onconnectionstatechange = () => {
        if (peer.connectionState === 'connected') {
          clearTimeout(giveUp);
          status = 'playing';
        }
        if (peer.connectionState === 'failed') unreachable();
      };
      giveUp = setTimeout(unreachable, PATIENCE);
      await peer.setLocalDescription(await peer.createOffer());
      await gathered(peer);
      const res = await fetch(`/api/devices/${encodeURIComponent(id)}/live`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ sdp: peer.localDescription.sdp }),
      });
      const body = await res.json().catch(() => ({}));
      if (res.status === 502) throw new Error(t('salon.impression.camera_injoignable'));
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      if (pc !== peer) return; // stopped meanwhile
      await peer.setRemoteDescription({ type: 'answer', sdp: body.sdp });
    } catch (e) {
      status = 'failed';
      reason = e.message;
    }
  }

  function stop() {
    clearTimeout(giveUp);
    if (pc) pc.close();
    pc = null;
    if (video) video.srcObject = null;
  }

  // A tab in the background holds no video open.
  function onVisibility() {
    if (document.visibilityState === 'visible') start();
    else stop();
  }

  onMount(() => {
    start();
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  });
  onDestroy(stop);
</script>

<div class="live" data-status={status}>
  <!-- svelte-ignore a11y_media_has_caption -->
  <video bind:this={video} autoplay muted playsinline></video>
  {#if status === 'connecting'}
    <span class="badge">{t('salon.impression.camera_connexion')}</span>
  {:else if status === 'playing'}
    <span class="badge on"><i></i>{t('salon.impression.camera_direct')}</span>
  {:else}
    <div class="failed">
      <Icon name="camera" size={22} />
      <span>{t('salon.impression.camera_indisponible')}</span>
      {#if reason}<small>{reason}</small>{/if}
      <button onclick={start}>{t('salon.impression.camera_reessayer')}</button>
    </div>
  {/if}
</div>

<style>
  .live {
    position: relative;
    aspect-ratio: 16 / 9;
    border-radius: var(--r-md);
    overflow: hidden;
    background: #000;
  }

  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }

  .badge {
    position: absolute;
    top: 10px;
    left: 10px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgb(0 0 0 / 0.55);
    color: #fff;
    font-size: 12px;
    font-weight: 650;
  }

  .badge.on i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #e5484d;
  }

  .failed {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 6px;
    padding: 16px;
    text-align: center;
    color: #fff;
    font-size: 14px;
  }

  .failed small {
    max-width: 36ch;
    opacity: 0.75;
    font-size: 12px;
  }

  .failed button {
    margin-top: 4px;
    border: 0;
    border-radius: 999px;
    padding: 7px 14px;
    font: inherit;
    font-weight: 650;
    background: rgb(255 255 255 / 0.15);
    color: #fff;
    cursor: pointer;
  }
</style>
