// What Moli says, sentence by sentence; how a conversation ends.

/** Sentences (numbers like 22.9 stay whole). The first stays alone so the
 *  voice starts sooner; a short one after it joins the previous. */
export function sentences(text) {
  const out = [];
  for (const s of text.trim().split(/(?<=[.!?…])\s+/).filter(Boolean)) {
    if (out.length > 1 && s.length < 24) out[out.length - 1] = `${out.at(-1)} ${s}`;
    else out.push(s);
  }
  return out;
}

// Words a person says, not words the interface shows: one list per language
// (the house's language, `i18n.language`, picks it; French when unknown).
const GOODBYE = {
  fr: /^(merci( beaucoup)?( moli)?|c['’]est tout( merci)?|stop|au revoir( moli)?|ça ira( merci)?|bonne nuit( moli)?|à plus)[\s.!]*$/i,
  en: /^(thanks( a lot)?(,? moli)?|thank you( very much)?(,? moli)?|that['’]s (all|it)(,? (thanks|thank you))?|that will do(,? thanks)?|stop|(goodbye|bye)(,? moli)?|good night(,? moli)?|see you( later)?)[\s.!]*$/i,
};

/** A whole utterance that only closes the conversation. */
export const isGoodbye = (text, language = 'fr') => (GOODBYE[language] ?? GOODBYE.fr).test(text.trim());

// An answer that already tells the person to approve on the screen.
const APPROVAL = { fr: /valid/i, en: /approv|confirm|valid/i };

/** Does `reply` already ask for an approval? */
export const mentionsApproval = (reply, language = 'fr') => (APPROVAL[language] ?? APPROVAL.fr).test(reply);

/** One sentence as the server says it (`player.playSpeech` plays it): the
 *  cloud voice streamed while it is made (PCM), else MP3 or WAV whole;
 *  `voice` to try another. */
export async function fetchSpeech(text, signal, voice) {
  const res = await fetch('/api/assistant/speak', {
    method: 'POST',
    headers: { 'content-type': 'application/json', accept: 'audio/pcm, audio/mpeg;q=0.9, audio/wav;q=0.8' },
    body: JSON.stringify(voice ? { text, voice } : { text }),
    signal,
  });
  if (!res.ok) throw new Error(`speak ${res.status}`);
  return res;
}
