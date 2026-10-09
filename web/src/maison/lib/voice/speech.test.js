import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sentences, isGoodbye, mentionsApproval } from './speech.js';

test('sentences, numbers kept whole', () => {
  assert.deepEqual(sentences('Il fait 22.9 °C dans le salon. Dehors, il pleut ! Bonne soirée.'), [
    'Il fait 22.9 °C dans le salon.',
    'Dehors, il pleut ! Bonne soirée.',
  ]);
  assert.deepEqual(sentences('Oui.'), ['Oui.']);
  assert.deepEqual(sentences('  '), []);
});

test('goodbyes end the conversation, questions do not', () => {
  for (const t of ['Merci.', 'merci Moli', "C'est tout", 'Stop !', 'Au revoir', 'ça ira merci']) assert.ok(isGoodbye(t), t);
  for (const t of ['Merci d’éteindre le salon', 'Stop la musique', 'Quel temps fait-il ?']) assert.ok(!isGoodbye(t), t);
});

test('goodbyes follow the house language, French when unknown', () => {
  for (const t of ['Thanks.', 'thank you Moli', "That's all", 'That’s it, thanks', 'Stop!', 'Goodbye', 'good night moli']) assert.ok(isGoodbye(t, 'en'), t);
  for (const t of ['Thanks for turning off the lights', 'Stop the music', 'What is the weather?']) assert.ok(!isGoodbye(t, 'en'), t);
  assert.ok(!isGoodbye('Merci', 'en'));
  assert.ok(isGoodbye('Merci', 'de'));
});

test('an answer that already asks for approval is recognised', () => {
  assert.ok(mentionsApproval('Il faut que tu valides.'));
  assert.ok(!mentionsApproval('C’est en attente.'));
  assert.ok(mentionsApproval('Please approve it on screen.', 'en'));
  assert.ok(!mentionsApproval('It is waiting.', 'en'));
});
